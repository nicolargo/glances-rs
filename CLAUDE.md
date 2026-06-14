# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`glances-rs` is a ground-up Rust reimplementation of a lightweight monitoring
server inspired by [Glances](https://github.com/nicolargo/glances) v5. It serves
a Glances-compatible REST API (`/api/5/...`) with the smallest possible CPU/RAM
footprint, shipped as a single binary. Pre-v1, under active development.

The footprint goal is the project's reason to exist — weigh every change against
CPU/RAM cost, not just correctness.

## Commands

Use the `Makefile` targets; they wrap `cargo` with the flags CI expects.

- `make build` — release binary at `target/release/glances-rs` (uses the
  footprint profile: `lto`, `codegen-units = 1`, `strip`, `panic = "abort"`).
- `make debug` / `make run` — fast debug build / run the server.
- `make test` — `cargo test --locked`.
- `make lint` — `cargo fmt --all --check` + `cargo clippy --all-targets -D warnings`.
- `make check` — full local CI pass (lint + test + build); run before pushing.
- Run one test: `cargo test status_responds_200` or a file's suite with
  `cargo test --test probes`.

CI (`.github/workflows/ci.yml`) gates on fmt, clippy (`-D warnings`), `cargo test`
+ release build across Linux/macOS/Windows, and `cargo audit`. `Cargo.lock` is
committed (this is a binary) — keep it in sync.

## Authoritative documents

These are the source of truth and are kept current — read them before changing
behavior, and update them when you change a contract:

- `ARCHITECTURE.md` — every design decision *and its rationale*. Sections are
  referenced throughout the code as `§N` (e.g. `§5.4`). When code cites a
  section, that section explains the constraint the code must honor.
- `docs/api.md` — the frozen API contract: routes, exact JSON payload shape per
  plugin, status-code semantics, config discovery order.
- `DEVELOPMENT_PLAN.md` — phased roadmap; records which phase resolved each
  open question.

## Architecture

Single binary crate. `main.rs` is tiny; `lib.rs::run()` parses args, loads
config, and calls `server::serve`.

**Lazy collection with wake-up (the core decision, §3).** Nothing collects while
no client is connected. Each plugin is independently *Idle* (no registry entry)
or *Active* (a loop task collecting at its refresh period). The first request to
an idle plugin wakes it and **waits for one full collection cycle** before
responding — the server never returns `null`/empty. A collector stops itself
after `idle_cycles` periods without a request. This satisfies both near-zero
idle footprint *and* correct rate metrics (which need two timed samples).

Module map:

- `collector.rs` — `ensure_plugin` (wake/wait, maps to `503` on timeout) and
  `plugin_loop`. The wake/idle race contract is documented at the top of the
  file: a request bumps `last_request` *before* taking the registry lock; a loop
  re-checks idleness *under* that lock. There is no external cancellation — the
  idle self-check is the stop mechanism, keeping the registry the single source
  of truth.
- `state.rs` — `AppState`, `Collector`. Three distinct sync primitives, each
  matched to its access pattern (§5.1); do not collapse them: Tokio `RwLock` for
  the store, `AtomicI64` for per-plugin `last_request`, `Mutex` for the
  collector registry.
- `plugins/mod.rs` — the `Plugin` trait and `PluginId`. Plugins are **stateless
  objects**: all inter-cycle memory lives in `Plugin::State`, owned by the loop
  task and passed to `collect()` by `&mut` — exclusive by construction, no lock.
- `plugins/{mem,cpu,load,network}.rs` — the four v1 plugins. `mem`/`load` are
  instantaneous (`State = ()`); `cpu`/`network` are rate plugins.
- `server.rs` — axum router construction, startup, and the §7.1 security check.
- `api/mod.rs` — route handlers. A single dynamic `/api/5/:plugin` route serves
  all plugins; `api/security.rs` holds auth + trusted-host middleware.

### Rules that are easy to break

- **Rate diffs (§5.4):** use `saturating_sub` (counters roll back on reboot/wrap),
  skip items absent from the previous sample (`?` on the lookup), and divide by
  *measured* elapsed `Instant` time, never the configured refresh.
- **Collection plugins (`network`, §8.1):** inter-cycle state must store **only
  the current sample** (`previous = Some(now)`), never a merge — otherwise dead
  interfaces leak forever. This is called out as a code comment; keep it.
- **Rate warm-up (§5.5):** rate plugins self-bootstrap on the cold path (sample,
  sleep `RATE_WARMUP` = 250 ms, sample) so the first response carries real rates.
  The sleep must respect `sysinfo`'s ~200 ms minimum CPU-refresh interval.
- **Probes are inert (§6.4):** `/status` and `/healthz` live outside the auth/
  wake middleware — always `200`, no auth, never wake a collector. Don't move
  them under the `/api/5/` sub-router.

### Security model (§7)

Closed by default: binds `127.0.0.1`; binding a non-loopback address **without a
password is a hard startup refusal**, not a warning. Auth is HTTP Basic only,
compared with `constant_time_eq`. CORS is an explicit allow-list (never `*`),
`Host` is validated against `trusted_hosts`. TLS is out of scope — delegated to a
reverse proxy. Routes serialize an explicit *public* view, never a raw internal
struct.

## Tests

Integration tests live in `tests/` (`probes.rs`, `security.rs`, `engine.rs`) and
drive the axum router via `tower::ServiceExt::oneshot` against
`build_router(AppState::new(config))` — no real socket. Unit tests sit inline in
their modules (`#[cfg(test)]`). Plugins are tested by feeding two samples to
`collect()` directly.
