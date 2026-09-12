# syntax=docker/dockerfile:1

# glances-rs container image.
#
# The final stage is `scratch`: the image holds the statically linked binary
# and a config file, and nothing else — no shell, no package manager, no
# shared libc, no CA bundle. There is no process to spawn but the server, and
# no library to patch: a CVE in the image can only be a CVE in the binary.
#
# `docker/config.toml` is baked in because a container that binds loopback is
# unreachable, and ARCHITECTURE.md §7.1 refuses a non-loopback bind without a
# password. See that file for the trade-offs it encodes.

# ---------------------------------------------------------------------------
# Stage 1 — build a static musl binary
# ---------------------------------------------------------------------------
# The alpine image's host target *is* musl, so this links statically with no
# cross-compilation setup. Under `docker buildx --platform`, each architecture
# builds natively against its own base image.
FROM rust:1.96-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# The cache mounts make rebuilds cheap but are not part of any layer, so the
# binary has to be copied out of `target/` inside the same RUN. They are keyed
# per architecture to keep concurrent multi-arch builds from colliding.
ARG TARGETARCH
RUN --mount=type=cache,id=cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-target-${TARGETARCH},target=/src/target,sharing=locked \
    cargo build --release --locked && \
    cp target/release/glances-rs /glances-rs

# ---------------------------------------------------------------------------
# Stage 2 — the image
# ---------------------------------------------------------------------------
FROM scratch

LABEL org.opencontainers.image.title="glances-rs" \
      org.opencontainers.image.description="Lightweight monitoring server with a Glances-compatible REST API" \
      org.opencontainers.image.source="https://github.com/nicolargo/glances-rs" \
      org.opencontainers.image.licenses="MIT"

COPY --from=builder /glances-rs /glances-rs
COPY docker/config.toml /etc/glances-rs/config.toml

# Numeric, because `scratch` has no /etc/passwd to resolve a name against.
# 65534 is `nobody` on every mainstream distro. Every file the plugins read
# (/proc/stat, /proc/meminfo, /proc/vmstat, /proc/diskstats, /sys/class/net,
# /etc/os-release) is world-readable, and port 61208 is unprivileged, so the
# server needs no capability at all — run it with `--cap-drop ALL`.
USER 65534:65534

EXPOSE 61208

ENTRYPOINT ["/glances-rs"]
