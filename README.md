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

## Security

`glances-rs` is **closed by default**: it binds to `127.0.0.1` and refuses
to start on a non-loopback address unless a password is configured
(ARCHITECTURE.md §7.1). Configuration lives in a TOML file (see
[`docs/glances-rs.example.toml`](docs/glances-rs.example.toml)); discovery
order and the full API contract are in [`docs/api.md`](docs/api.md).

- **Authentication** — HTTP Basic, compared in constant time. Set
  `[server].password`; clients send `Authorization: Basic <base64>` (any
  username, the password is what's checked).
- **CORS** — an explicit allow-list (`[security].cors_origins`), empty by
  default. Never a wildcard.
- **Trusted host** — the `Host` header is validated against
  `[security].trusted_hosts` (default `localhost`, `127.0.0.1`).

> **TLS must be terminated by a reverse proxy** (nginx, Caddy, …). The
> binary speaks plain HTTP only; Basic auth sends a base64-encoded — *not*
> encrypted — password, so any non-loopback exposure must sit behind a TLS
> proxy (ARCHITECTURE.md §7.5).

## License

[MIT](LICENSE)
