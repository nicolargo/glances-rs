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

## Docker

The image is built `FROM scratch`: it contains the statically linked binary
and one config file, and nothing else — no shell, no package manager, no
shared libc, no CA bundle. Total **1.94 MB**, measured RSS **1.3 MiB** at
rest. There is no second process to spawn and no library to patch: a CVE in
the image can only be a CVE in glances-rs itself.

Published to GHCR on every version tag, as a `linux/amd64` + `linux/arm64`
manifest list:

```bash
docker pull ghcr.io/nicolargo/glances-rs:latest
```

Or build it yourself:

```bash
make docker-build                             # or: docker build -t glances-rs .
echo "GLANCES_RS_PASSWORD=$(openssl rand -base64 24)" > .env
docker compose up -d
curl -u glances:"$GLANCES_RS_PASSWORD" http://localhost:61208/api/5/mem
```

Each release carries a SLSA provenance attestation and an SBOM, both built by
`.github/workflows/release.yml` and verifiable before you run anything:

```bash
gh attestation verify oci://ghcr.io/nicolargo/glances-rs:latest -R nicolargo/glances-rs
docker buildx imagetools inspect ghcr.io/nicolargo/glances-rs:latest --format '{{ json .SBOM }}'
```

Tags are `X.Y.Z`, `X.Y` and `latest`. There is deliberately no bare-major tag:
the project is pre-v1, and `0` would promise a compatibility guarantee that
0.x does not make.

The username in `-u` is ignored — the config model is password-only.

### Running it against the host

The plugins read `/proc`, `/sys/class/net` and `/etc/os-release` through
hard-coded paths, so the container has to borrow the host's namespaces rather
than measure its own:

```bash
docker run -d --name glances-rs \
  --network host --pid host \
  --read-only --cap-drop ALL --security-opt no-new-privileges:true \
  -v /etc/os-release:/etc/os-release:ro \
  -e GLANCES_RS_PASSWORD \
  glances-rs:latest
```

| Flag | Why |
|---|---|
| `--network host` | `/sys/class/net` then lists the host's interfaces, and the container inherits the host's hostname (reported by `system` and by alert events). The server binds the host's port directly, so no `-p`. |
| `--pid host` | No effect on today's plugins — `/proc/stat`, `/proc/meminfo`, `/proc/vmstat` and `/proc/diskstats` are not namespaced — but correct for any future per-process plugin. |
| `-v /etc/os-release:ro` | `system` reports `linux_distro` from it. A scratch image has no such file; without the mount the field is simply absent (the plugin degrades, it does not fail). |
| `--read-only`, `--cap-drop ALL`, `no-new-privileges` | Free: glances-rs writes nothing to disk, every file it reads is world-readable, and port 61208 is unprivileged. The container runs as uid 65534. |

`-e TZ` is not needed: every timestamp glances-rs emits is UTC by
construction, and nothing in the code reads `TZ` or `/etc/localtime`.

### What the image changes, and what it does not

`docker/config.toml` is baked in at `/etc/glances-rs/config.toml`, the last
entry in the config discovery order. Mount your own file over that path — or
point `GLANCES_RS_CONFIG` elsewhere — to replace it entirely. It sets three
things:

- **`bind = "0.0.0.0"`.** A container that binds loopback is unreachable.
- **`password_env = "GLANCES_RS_PASSWORD"`.** §7.1 makes a password mandatory
  on a non-loopback bind, and an unset or empty variable is a hard startup
  error. The container **exits** rather than serving unauthenticated — the
  one behaviour to keep in mind when deploying it.
- **`trusted_hosts = []`.** This is the one place the image is *less* strict
  than a bare run. The default is `["localhost", "127.0.0.1"]`, the §7.4
  guard against DNS rebinding; an image cannot know the hostname or LAN
  address of the machine it will run on, so that default would answer
  `400 host not allowed` to every request not made from the host itself.
  Basic auth still gates every `/api/5` route, so this is not an open server
  — but it is one layer fewer. Put it back by mounting a config that lists
  the names you actually use.

### Known limitation: `fs` in a container

The `fs` plugin reads the *container's* mount table. The container's `/` is
the overlay, whose size and usage are those of the host filesystem backing
`/var/lib/docker` — on a standard install, the host root — so **the root
filesystem is reported correctly**. Other host filesystems (a separate
`/home` or `/data`, a ZFS pool) are not in the container's mount namespace
and are therefore missing.

Adding `-v /:/rootfs:ro` brings them in, at a real cost: the entire host
filesystem becomes readable from inside the container, which is most of what
the scratch base image was bought to avoid. `docker/config.toml` carries the
`hide`/`alias` recipe to make that output readable if you accept the trade.

### Deliberately not in the image

- **No `HEALTHCHECK`.** A scratch image has no shell and no `curl` to run one
  with, and adding either would undo the base image. Probe `/status` from
  outside: it is inert by design (§6.4) — always `200`, no auth, and it never
  wakes a collector.
- **No `/var/run/docker.sock` mount.** There is no `containers` plugin yet, so
  it would buy nothing today — and access to that socket is equivalent to root
  on the host, which would undo every hardening flag above. The same goes for
  the rootless Podman socket.

## Footprint

The whole reason glances-rs exists is to serve the same API with a far
smaller footprint than the Python original. Measured **on the same machine,
with the same nine plugins**, using
[`scripts/footprint.sh`](scripts/footprint.sh) — a `/proc`-based sampler of
resident memory and CPU under a rate-controlled polling load on
`/…/all` (2 req/s is the default Glances WebUI/TUI refresh; 10 and 100 req/s
stand in for heavier polling):

| Polling load | glances-rs RSS | glances-rs CPU | Glances RSS | Glances CPU |
|---|---|---|---|---|
| at rest (no client) | **≈ 3.8 MiB** | ≈ 0 % | ≈ 107 MiB | collects continuously |
| 2 req/s  | 3.8 MiB | 0.20 % | 107.5 MiB | 1.80 % |
| 10 req/s | 4.0 MiB | 0.50 % | 109 MiB | 6.50 % |
| 100 req/s | 5.5 MiB | 1.60 % | 115.7 MiB | 11.0 % |

Glances was run with the exact same scope —
`glances --disable-plugins all --enable-plugins
cpu,load,mem,network,system,uptime,memswap,fs,diskio --disable-history
--disable-webui -w`. Even like-for-like, glances-rs uses **~28× less memory**
at rest (~21× under the heaviest polling) and a fraction of the CPU. The binary
is a single 2.1 MiB file vs a Python install (interpreter + FastAPI/uvicorn/
psutil + ~30 deps).

Two design choices drive this: a compiled, GC-free runtime, and **lazy
collection** — glances-rs collects nothing while no client is connected and
stays near its idle RSS until polled hard, whereas Glances' scheduler runs
continuously (the footprint weakness its own v5 architecture document
acknowledges). Most of the gap is the Python+framework baseline: even at rest,
scoped to the very same nine plugins, Glances sits ~28× above glances-rs before
a single request is served.

> **Honest caveats.** Numbers come from one machine, not your server —
> treat them as indicative and run the script on your target. The
> comparison uses Glances **4.5.5 stable** (REST API v4), not the
> `develop-v5` branch.

```sh
# Reproduce (Linux): start each server, then, on the same machine:
scripts/footprint.sh "$(pgrep -n glances-rs)" http://127.0.0.1:61208/api/5/all "2 10 100"
```

## License

[MIT](LICENSE)
