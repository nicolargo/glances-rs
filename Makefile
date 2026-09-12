# glances-rs — development shortcuts.
# `make build` produces the release binary at target/release/glances-rs.

CARGO ?= cargo
BINARY = target/release/glances-rs
IMAGE ?= glances-rs
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

.PHONY: build debug run test lint fmt check clean docker-build docker-run

## Build the optimized release binary (footprint profile from Cargo.toml)
build:
	$(CARGO) build --release --locked
	@ls -lh $(BINARY) | awk '{print "Binary: " $$9 " (" $$5 ")"}'

## Build the debug binary (faster compile, for development)
debug:
	$(CARGO) build

## Run the server (debug build)
run:
	$(CARGO) run

## Run the test suite
test:
	$(CARGO) test --locked

## Check formatting and lints (same gates as CI)
lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets -- -D warnings

## Format the code in place
fmt:
	$(CARGO) fmt --all

## Full local CI pass: lint + tests + release build
check: lint test build

## Remove build artifacts
clean:
	$(CARGO) clean

## Build the container image (scratch base, ~2 MB)
docker-build:
	docker build -t $(IMAGE):$(VERSION) -t $(IMAGE):latest .
	@docker images $(IMAGE):$(VERSION) --format 'Image: {{.Repository}}:{{.Tag}} ({{.Size}})'

## Run the container against the host. Needs GLANCES_RS_PASSWORD in the
## environment -- the image binds 0.0.0.0 and refuses to start without one.
docker-run:
	docker run --rm --name glances-rs \
	  --network host --pid host \
	  --read-only --cap-drop ALL --security-opt no-new-privileges:true \
	  -v /etc/os-release:/etc/os-release:ro \
	  -e GLANCES_RS_PASSWORD \
	  $(IMAGE):latest
