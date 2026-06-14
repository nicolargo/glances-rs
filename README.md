# glances-rs

A lightweight monitoring server, inspired by [Glances](https://github.com/nicolargo/glances)
v5, reimplemented from the ground up in Rust. It exposes the same observable
REST API (`/api/5/...`) with the smallest possible CPU and RAM footprint:
no collection runs while no client is connected (lazy collection with
wake-up), and the whole server ships as a single binary for Linux (primary
target), macOS and Windows.

**Status: pre-v1, under active development.**

- Design rationale and key decisions: [ARCHITECTURE.md](ARCHITECTURE.md)
- Implementation roadmap: [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md)

## Build

```sh
make build        # release binary at target/release/glances-rs
```

Other targets: `make test`, `make lint`, `make check` (full local CI pass).
Without `make`: `cargo build --release`.

## Quick start

Run the binary — no configuration is required to try it locally:

```sh
glances-rs
```

It listens on `http://127.0.0.1:61208`. From another terminal:

```sh
curl http://127.0.0.1:61208/api/5/mem          # one plugin
curl http://127.0.0.1:61208/api/5/all          # every plugin at once
curl http://127.0.0.1:61208/api/5/pluginslist  # cpu, diskio, fs, load, mem, memswap, network, system, uptime
```

The first request to a plugin wakes its collector and waits for one
collection cycle, so you always get real data (never `null`). When no one
queries a plugin for a while, its collector stops on its own — that is the
lazy design that keeps the footprint near zero at rest. `GET /status` and
`GET /healthz` are liveness probes: always `200`, no auth, and they never
wake a collector.

Logging is controlled with `RUST_LOG` (e.g. `RUST_LOG=debug glances-rs`).

## Configuration

Configuration is an optional TOML file. glances-rs looks for it, in order:

1. `--config <path>` (or `-c <path>`)
2. the `GLANCES_RS_CONFIG` environment variable
3. `./glances-rs.toml`
4. `$XDG_CONFIG_HOME/glances-rs/config.toml` (`~/.config/...`)
5. `/etc/glances-rs/config.toml`

The first match wins; with no file found, the built-in defaults apply. A
path given via `--config` or `GLANCES_RS_CONFIG` that does not exist is a
startup error (no silent fallback). A fully commented example is in
[`docs/glances-rs.example.toml`](docs/glances-rs.example.toml); the full
API contract is in [`docs/api.md`](docs/api.md).

## Securing the server

glances-rs is **closed by default** and walks you through opening it
safely, one step at a time.

### 1. Local use needs nothing

Out of the box the server binds to `127.0.0.1` (loopback): it is only
reachable from the same machine, so no password is required. This is the
safe default — you can stop here for local monitoring.

### 2. Exposing it on the network requires a password

To bind to a routable address you **must** set a password, or the server
**refuses to start** (a hard error, not a warning):

```toml
[server]
bind = "0.0.0.0"   # reachable from the network
```

```text
$ glances-rs
glances-rs: refusing to start: bind address 0.0.0.0 is reachable from the
network but no password is configured. Set [server].password, or bind to a
loopback address (ARCHITECTURE.md §7.1)
```

### 3. Set the password without writing it in the config file

Putting a cleartext password in a config file is a bad habit (it gets
committed, backed up, copied around). Instead, the config names an
**environment variable** that holds the secret — the file only ever stores
the variable's *name*:

```toml
[server]
bind = "0.0.0.0"
password_env = "GLANCES_RS_PASSWORD"   # the NAME of the variable, not the secret
```

glances-rs reads `GLANCES_RS_PASSWORD` at startup. If it is unset or empty,
the server refuses to start — it never silently runs without auth.

Now choose how that variable gets set:

**a. Local development — a quick shell export**

```sh
export GLANCES_RS_PASSWORD='choose-a-strong-secret'
glances-rs
```

**b. Production with systemd — an `EnvironmentFile` (a `.env` file)**

Put the secret in a file readable only by the service account, *outside*
your project directory and version control:

```sh
sudo install -m 600 /dev/stdin /etc/glances-rs/glances-rs.env <<'EOF'
GLANCES_RS_PASSWORD=choose-a-strong-secret
EOF
```

Reference it from the unit — this `.env`-format file is loaded by systemd,
not by glances-rs:

```ini
# /etc/systemd/system/glances-rs.service
[Service]
ExecStart=/usr/local/bin/glances-rs --config /etc/glances-rs/config.toml
EnvironmentFile=/etc/glances-rs/glances-rs.env
DynamicUser=yes
```

> The `chmod 600` (or a dedicated service user) is what protects the
> secret. A `.env` file is still a cleartext file — keep it off the repo
> and lock down its permissions.

**c. Containers — Docker / Compose secrets**

```sh
docker run -e GLANCES_RS_PASSWORD=... ...    # simple
```

```yaml
# docker-compose — env_file keeps the secret out of the compose file
services:
  glances-rs:
    image: glances-rs
    env_file: [glances-rs.env]   # add it to .gitignore
```

### 4. Connecting with a password

Clients send HTTP Basic credentials. The username is ignored; only the
password is checked (in constant time):

```sh
curl -u any:choose-a-strong-secret http://server:61208/api/5/all
```

### 5. TLS — always use a reverse proxy when exposed

The binary speaks **plain HTTP only**. Basic auth sends a base64-encoded —
**not encrypted** — password, so anyone on the wire could read it. Any
non-loopback exposure must sit behind a TLS-terminating reverse proxy
(nginx, Caddy, Traefik, …), which also lets you keep glances-rs bound to
loopback and reachable only through the proxy.

### 6. Browser dashboards (CORS) and host checks

- **CORS** is an explicit allow-list, empty by default (no cross-origin
  browser access). Add the dashboard's origin only if needed:
  ```toml
  [security]
  cors_origins = ["https://dashboard.example.com"]   # never "*"
  ```
- **Trusted host** — a request's `Host` header must match
  `[security].trusted_hosts` (default `["localhost", "127.0.0.1"]`); add
  your public hostname when exposing the server. This blocks spoofed-`Host`
  attacks.

## Footprint

The whole reason glances-rs exists is to serve the same API with a far
smaller footprint than the Python original. Measured **on the same machine,
with the same four plugins**, using
[`scripts/footprint.sh`](scripts/footprint.sh) — a `/proc`-based sampler of
resident memory and CPU under a rate-controlled polling load on
`/…/all` (2 req/s is the default Glances WebUI/TUI refresh; 10 and 100 req/s
stand in for heavier polling):

| Polling load | glances-rs RSS | glances-rs CPU | Glances RSS | Glances CPU |
|---|---|---|---|---|
| at rest (no client) | **≈ 4.5 MiB** | ≈ 0 % | ≈ 69 MiB | collects continuously |
| 2 req/s  | 4.7 MiB | 0.25 % | 69 MiB | 0.50 % |
| 10 req/s | 4.9 MiB | 0.25 % | 69 MiB | 1.25 % |
| 100 req/s | 5.6 MiB | 2.25 % | 69 MiB | 9.0 % |

Glances was run with the exact same scope —
`glances --disable-plugins all --enable-plugins cpu,load,mem,network
--disable-history --disable-webui -w`. Even like-for-like, glances-rs uses
**~15× less memory** and a fraction of the CPU. The binary is a single
2.1 MiB file vs a Python install (interpreter + FastAPI/uvicorn/psutil +
~30 deps).

Two design choices drive this: a compiled, GC-free runtime, and **lazy
collection** — glances-rs collects nothing while no client is connected and
its memory barely moves under load, whereas Glances' scheduler runs
continuously (the footprint weakness its own v5 architecture document
acknowledges). The memory gap is the Python+framework baseline, not the
plugin count: scoping Glances to four plugins barely changed its RSS.

> **Honest caveats.** Numbers come from one container, not your server —
> treat them as indicative and run the script on your target. The
> comparison uses Glances **4.5.5 stable**, not the `develop-v5` branch.

```sh
# Reproduce (Linux): start each server, then, on the same machine:
scripts/footprint.sh "$(pgrep -n glances-rs)" http://127.0.0.1:61208/api/5/all "2 10 100"
```

## License

[MIT](LICENSE)
