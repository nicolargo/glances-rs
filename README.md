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

## License

[MIT](LICENSE)
