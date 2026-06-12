# glances-rs — development shortcuts.
# `make build` produces the release binary at target/release/glances-rs.

CARGO ?= cargo
BINARY = target/release/glances-rs

.PHONY: build debug run test lint fmt check clean

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
