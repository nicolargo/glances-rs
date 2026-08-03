# Footprint pass (Phase 9) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Cut per-request CPU/allocation and binary size without changing any API payload or the §3 lazy contract — the two measured wins from the Phase 9 baseline study, plus a build-profile trial.

**Architecture:** (1) Drop `async-trait` for native RPITIT async in the `Plugin` trait (removes per-cycle future boxing + one dependency). (2) Serialize each plugin's payload **once per cycle** and store the bytes, so requests serve pre-serialized bytes instead of deep-cloning + re-serializing a `Value` per request. (3) Trial `opt-level="s"` and adopt only if runtime RSS/CPU do not regress.

**Tech Stack:** Rust 2024 (rust-version 1.94), axum 0.8, serde_json, `bytes` (already transitive via axum), tokio `current_thread`.

## Global Constraints

- **Footprint is the project's reason to exist.** Every change must be measured; adopt only on a measured win. The default config hot path (`/all` under load) is the target.
- **No API/payload change.** Response bytes for every route stay byte-identical (same JSON). The integration suite (`tests/*.rs`) is the behavioural gate — it must pass unchanged.
- **§3 lazy contract untouched.** No change to wake/idle, `ensure_plugin`'s wait-for-first-cycle, or the retain-on-stop store semantics. Collectors still self-stop; the store still survives stops.
- **Alerting order preserved.** `observe()` still mutates the `Value` (adds `_levels`) BEFORE serialization/publish — serialize only after `observe`.
- **Baseline to beat (v0.4.0, `opt-level=3`):** rest 4.0 MiB; 100 req/s 5.3 MiB / 1.8% CPU; binary 2.233 MiB. Working notes: `docs/footprint-baseline-phase9.md`.
- Run `cargo fmt --all` before every commit; `make lint` (clippy `-D warnings`) + `make test` gate each task. `Cargo.lock` stays in sync.
- Commit-message trailer on every commit:
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r`.

---

## Task 1: Drop `async-trait` → native RPITIT async (H2 bonus)

Remove `#[async_trait]` from the `Plugin` trait and all nine impls; the trait's
`collect` becomes a native `-> impl Future<Output = Value> + Send`. `plugin_loop<P: Plugin>`
is generic (monomorphised in `spawn_plugin`), `Plugin` is never used as `dyn`,
so RPITIT works and the concrete futures stay `Send` for `tokio::spawn`.

**Files:**
- Modify: `src/plugins/mod.rs` (the `Plugin` trait)
- Modify: `src/plugins/{cpu,mem,load,network,system,uptime,memswap,fs,diskio}.rs` (drop `#[async_trait]` attr on each `impl Plugin`)
- Modify: `Cargo.toml` (remove `async-trait`), `Cargo.lock`

**Interfaces:**
- Produces: `Plugin::collect` signature `fn collect(&self, state: &mut Self::State) -> impl Future<Output = serde_json::Value> + Send;`
- Consumes: nothing new.

- [ ] **Step 1: Change the trait** in `src/plugins/mod.rs`

Remove the `#[async_trait::async_trait]` attribute on the `trait Plugin`, and change the `collect` method from `async fn collect(...)` to:

```rust
fn collect(&self, state: &mut Self::State) -> impl std::future::Future<Output = serde_json::Value> + Send;
```

Keep the trait's supertraits (`Send + Sync + 'static`) and `type State: Default + Send` exactly as they are. Leave `id()` / `refresh()` unchanged.

- [ ] **Step 2: Drop the attribute on every impl**

In each of the nine plugin files, remove the `#[async_trait::async_trait]` line above `impl Plugin for XxxPlugin`. Leave the method body as `async fn collect(&self, state: &mut Self::State) -> Value { ... }` — an `async fn` in an impl satisfies the RPITIT `-> impl Future + Send` trait method (Rust 1.75+). Change nothing else in the bodies.

- [ ] **Step 3: Remove the dependency**

In `Cargo.toml` delete the `async-trait = "0.1.89"` line. If any file still has `use async_trait::async_trait;`, remove it.

- [ ] **Step 4: Build + test**

Run: `cargo build 2>&1 | tail -20`
Expected: compiles. If the compiler reports a `collect` future is not `Send` for some plugin, that plugin holds a non-`Send` value across an `.await` — report it (do NOT add `async-trait` back); it almost certainly does not (they were `Send` under async-trait's boxing already).

Run: `make lint && make test`
Expected: fmt clean, clippy clean, all tests green (behaviour unchanged).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/plugins Cargo.toml Cargo.lock
git commit -m "perf(plugins): drop async-trait for native RPITIT async

Plugin::collect is now -> impl Future + Send; plugin_loop is generic and
Plugin is never dyn, so no boxing is needed. Removes a per-cycle future
allocation and one dependency. No behaviour change.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Task 2: Pre-serialize payload once per cycle (H2 core)

Store each plugin's payload as pre-serialized `bytes::Bytes`, produced once per
collection cycle (after `observe`). The single-plugin route serves those bytes
zero-copy; `/all` concatenates the parts into one JSON object without
re-parsing or re-serializing any plugin.

**Files:**
- Modify: `Cargo.toml` (add `bytes` as a direct dep — already transitive via axum; pin to axum's version, e.g. `bytes = "1"`)
- Modify: `src/state.rs` (store `HashMap<PluginId, Bytes>`; `publish`/`snapshot` types)
- Modify: `src/collector.rs` (`ensure_plugin` returns `Bytes`; `plugin_loop` serializes once after `observe`)
- Modify: `src/api/mod.rs` (handlers serve `Bytes`; `/all` composes)

**Interfaces:**
- Consumes: `AppState.alerts.observe(&config, id, &mut value)` (unchanged, mutates the `Value` in place before serialization).
- Produces: `AppState::publish(&self, id, body: Bytes)`, `AppState::snapshot(&self, id) -> Option<Bytes>`, `ensure_plugin(...) -> Result<Bytes, EnsureError>`.

- [ ] **Step 1: Add the `bytes` dependency**

In `Cargo.toml` `[dependencies]`, add `bytes = "1"` (confirm the resolved version matches what axum already pulls, so no new version enters `Cargo.lock`). Run `cargo build` to refresh the lock.

- [ ] **Step 2: Change the store type** in `src/state.rs`

- `use bytes::Bytes;`
- `store: RwLock<HashMap<PluginId, Bytes>>`
- `pub async fn publish(&self, id: PluginId, body: Bytes) { self.store.write().await.insert(id, body); }`
- `pub async fn snapshot(&self, id: PluginId) -> Option<Bytes> { self.store.read().await.get(&id).cloned() }` (a `Bytes` clone is a cheap refcount bump, not a deep copy — this is the win.)

- [ ] **Step 3: Serialize once per cycle** in `src/collector.rs`

In `plugin_loop`, after `collect()` and `observe()`, serialize the finalized `Value` to bytes exactly once, then publish:

```rust
let mut value = plugin.collect(&mut state).await;
app.alerts.observe(&app.config, id, &mut value);
let body = Bytes::from(serde_json::to_vec(&value).expect("Value serializes"));
app.publish(id, body).await;
```

Change `ensure_plugin`'s return type from `Result<Value, EnsureError>` to `Result<Bytes, EnsureError>`; it returns `app.snapshot(id).await` mapped as before (the wait-for-first-cycle and timeout logic is unchanged — only the payload type changes).

- [ ] **Step 4: Serve bytes in the handlers** in `src/api/mod.rs`

`plugin_stats`: on `Ok(body)` return the bytes with an explicit JSON content-type (zero-copy):

```rust
use axum::http::header;
// ...
Ok(body) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
```

(`Bytes` implements `IntoResponse`; the header tuple sets the content-type. The `404`/`503` arms are unchanged.)

`all_stats`: compose the aggregate object from the parts without re-serializing. Collect `(name, Bytes)` for every registered plugin whose wake succeeded, sort by name (to match the previous BTreeMap key order), and build one JSON object:

```rust
async fn all_stats(State(app): State<Arc<AppState>>) -> Response {
    let mut set = JoinSet::new();
    for id in PluginId::ALL {
        if !app.is_registered(id) { continue; }
        let app = app.clone();
        set.spawn(async move { (id, ensure_plugin(&app, id).await) });
    }
    let mut parts: Vec<(&'static str, Bytes)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok((id, Ok(body))) = joined {
            parts.push((id.as_str(), body));
        }
    }
    parts.sort_by_key(|(name, _)| *name);

    let mut out = Vec::with_capacity(2 + parts.iter().map(|(n, b)| n.len() + b.len() + 4).sum::<usize>());
    out.push(b'{');
    for (i, (name, body)) in parts.iter().enumerate() {
        if i > 0 { out.push(b','); }
        out.push(b'"');
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b"\":");
        out.extend_from_slice(body);
    }
    out.push(b'}');
    ([(header::CONTENT_TYPE, "application/json")], Bytes::from(out)).into_response()
}
```

(Each `body` is already a valid JSON object, so this yields a valid object `{"cpu":{…},"fs":{…},…}`. Remove the now-unused `Map`/`Json` imports if they are no longer referenced elsewhere in the file — `plugin_info` and `alert_history` may still use `Json`/`Map`, so check before deleting imports.)

- [ ] **Step 5: Build + test**

Run: `make lint && make test`
Expected: all integration tests green — they parse the response JSON, which is byte-for-byte the same structure. If any test compared against `Json(...)` internals, it still parses the body. Pay attention to `tests/engine.rs` (the wake/store contract) and `/all` tests.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add Cargo.toml Cargo.lock src/state.rs src/collector.rs src/api/mod.rs
git commit -m "perf(api): serialize each plugin payload once per cycle

Store pre-serialized Bytes instead of a Value; requests serve the cached
bytes (refcount clone) instead of deep-cloning + re-serializing per request.
/all concatenates the parts without re-parsing. No payload change.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Task 3: Build-profile trial `opt-level="s"` (H3)

Measure `opt-level="s"` against the current `3` on binary size AND runtime
RSS/CPU. Adopt only if size shrinks meaningfully with no runtime regression.

**Files:**
- Modify (conditionally): `Cargo.toml` `[profile.release] opt-level`

**Decision rule (apply exactly):** adopt `"s"` iff, on the same machine,
`/all` 100 req/s **CPU stays within +0.15 pt** of the `opt-level=3` baseline
(1.8%) AND peak RSS stays within the ±0.5 MiB noise band. If CPU regresses
beyond that, REVERT to `3` and record the numbers (size is not worth a CPU
cost in a CPU-sensitive server). Do NOT trial `"z"` unless `"s"` is adopted
and you want to check whether `"z"`'s extra size cut is free — that is
optional and only if `"s"` showed no regression.

- [ ] **Step 1: Baseline the current binary** (opt-level 3)

`make build`; record `ls -l target/release/glances-rs` size and run the footprint script (idle first, then `scripts/footprint.sh "$(pgrep -n glances-rs)" http://127.0.0.1:<port>/api/5/all`; use an alternate port via a throwaway `-c` config if 61208 is taken). Record rest + 100 req/s RSS/CPU. This is the post-Task-1/2 baseline.

- [ ] **Step 2: Trial `opt-level="s"`**

Edit `Cargo.toml` `[profile.release] opt-level = 3` → `"s"`. `make build`. Record binary size and re-run the footprint script identically.

- [ ] **Step 3: Apply the decision rule**

- If within thresholds: keep `"s"`, refresh `Cargo.lock` if needed, and commit.
- If it regresses CPU: `git checkout Cargo.toml`, rebuild at `3`, and record in the report that `"s"` was rejected with the numbers.

Either way, capture both binaries' sizes and both runs' RSS/CPU in the task report for Task 4's audit.

- [ ] **Step 4: Commit (only if adopted)**

```bash
git add Cargo.toml
git commit -m "build: opt-level=\"s\" release profile (-NN% binary, no runtime regression)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

If rejected, make no commit for this task and note the rejection for Task 4.

---

## Task 4: Re-baseline audit + version bump

**Files:**
- Modify: `Cargo.toml` (version 0.4.0 → 0.4.1), `Cargo.lock`
- Create: `docs/footprint-audit-v0.4.1.md`
- Remove: `docs/footprint-baseline-phase9.md` (working notes; folded into the audit)
- Modify: `DEVELOPMENT_PLAN.md` (mark Phase 9 items resolved; correct the stale `cpu`+`system` `/proc/stat` claim)

- [ ] **Step 1: Version bump**

`Cargo.toml` `version = "0.4.0"` → `"0.4.1"`; `cargo build` to refresh `Cargo.lock`.

- [ ] **Step 2: Final footprint measurement**

`make build`; run `scripts/footprint.sh` (idle-first, alternate port if needed) for rest + 2/10/100 req/s on `/all`; `ls -l` the binary. These are the shipped v0.4.1 numbers.

- [ ] **Step 3: Write `docs/footprint-audit-v0.4.1.md`**

Mirror `docs/footprint-audit-v0.4.0.md` structure. Report the v0.4.1 numbers vs the v0.4.0 baseline (rest 4.0 MiB, 100 req/s 5.3 MiB/1.8%, binary 2.233 MiB). Attribute the wins: per-cycle serialization + `Bytes` serving (per-request CPU/alloc), async-trait removal (per-cycle future alloc + one dep), and the `opt-level` outcome (adopted `"s"` with −NN% binary, or rejected with the numbers). State honestly which changes moved the needle and by how much, with the ±0.5 MiB caveat. Fold in the relevant working-notes findings, then remove `docs/footprint-baseline-phase9.md` (it is **untracked** — plain `rm`, not `git rm`).

- [ ] **Step 4: Update `DEVELOPMENT_PLAN.md`**

Mark Phase 9's four items resolved: per-cycle allocation (adopted), async-trait (adopted), build profile (adopted/rejected per Task 3), shared sampler (rejected — measured negligible; CORRECT the stale claim that `cpu`+`system` share `/proc/stat`: only `cpu` reads it), dependency audit (rejected — 76 crates, no duplicates, `sysinfo` subtree trivial).

- [ ] **Step 5: Gate + commit**

```bash
cargo fmt --all
make lint && make test
rm -f docs/footprint-baseline-phase9.md   # untracked working notes
git add Cargo.toml Cargo.lock docs/footprint-audit-v0.4.1.md DEVELOPMENT_PLAN.md
git commit -m "docs: v0.4.1 footprint pass audit + Phase 9 resolution

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Self-review notes (for the executor)

- **Behavioural neutrality:** Tasks 1 and 2 must not change any response byte — the integration suite is the gate. Task 2's `/all` composition must produce the same key order (sorted) and same per-plugin bodies as the old `Json(Map)` path.
- **Type consistency:** `ensure_plugin`, `snapshot`, `publish` all move from `Value` to `Bytes` together (Task 2) — a partial change will not compile. `plugin_stats` and `all_stats` both consume `Bytes`.
- **Measurement honesty (Tasks 3-4):** report real numbers; the ±0.5 MiB run-to-run caveat applies; adopt `opt-level` only per the decision rule.
- **Deferred/rejected are recorded, not silent:** H1 and H4 rejections and the stale-plan correction land in `DEVELOPMENT_PLAN.md` (Task 4).
