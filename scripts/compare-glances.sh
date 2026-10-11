#!/usr/bin/env bash
# System test: start Glances v5 (reference, `develop` branch) and glances-rs side
# by side, then diff their REST APIs with tests/system/compare_glances.py.
#
#   scripts/compare-glances.sh            # clone/venv in $WORKDIR, build, compare
#   GLANCES_SRC=~/glances scripts/compare-glances.sh   # reuse a local checkout
#   GLANCES_REF=<branch|tag|sha>          # reference revision (default: develop)
#
# Exit status: 0 = no unreviewed difference, 1 = new differences, 2 = setup error.
# Requires: git, python3 (>= 3.9, with venv), cargo, curl.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORKDIR=${WORKDIR:-${TMPDIR:-/tmp}/glances-rs-compare}
GLANCES_REF=${GLANCES_REF:-develop}
GLANCES_SRC=${GLANCES_SRC:-$WORKDIR/glances}
REF_PORT=${REF_PORT:-61208}
RS_PORT=${RS_PORT:-61209}
mkdir -p "$WORKDIR"

if [ ! -d "$GLANCES_SRC/.git" ]; then
  git clone --depth 1 -b "$GLANCES_REF" https://github.com/nicolargo/glances.git "$GLANCES_SRC"
fi
if [ ! -x "$WORKDIR/venv/bin/python" ]; then
  python3 -m venv "$WORKDIR/venv"
  "$WORKDIR/venv/bin/pip" install -q psutil requests jinja2 shtab fastapi uvicorn "python-jose[cryptography]"
fi
cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"

printf '[server]\nport = %s\n' "$RS_PORT" > "$WORKDIR/glances-rs.toml"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT

"$WORKDIR/venv/bin/python" "$GLANCES_SRC/run.py" -w --disable-webui -B 127.0.0.1 -p "$REF_PORT" \
  >"$WORKDIR/glances.log" 2>&1 & pids+=($!)
"$ROOT/target/release/glances-rs" -c "$WORKDIR/glances-rs.toml" \
  >"$WORKDIR/glances-rs.log" 2>&1 & pids+=($!)

for port in "$REF_PORT" "$RS_PORT"; do
  for _ in $(seq 60); do
    curl -fs "http://127.0.0.1:$port/api/5/pluginslist" >/dev/null && continue 2
    sleep 0.5
  done
  echo "server on port $port did not start; see logs in $WORKDIR" >&2
  exit 2
done

python3 -I "$ROOT/tests/system/compare_glances.py" \
  --glances "http://127.0.0.1:$REF_PORT" --glances-rs "http://127.0.0.1:$RS_PORT" "$@"
