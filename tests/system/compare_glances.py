#!/usr/bin/env python3
"""System test: compare the glances-rs REST API against a reference Glances v5.

Both servers must already be running (see ``scripts/compare-glances.sh`` which
sets everything up).  The comparison is *structural*: values are machine- and
time-dependent, so only routes, status codes, key sets, JSON types and the
``/info`` field schema are compared.

Every difference gets a stable id (``<scope>:<kind>:<detail>``).  Differences
whose id is listed in ``known_differences.json`` have been reviewed and
accepted (the doc reference is in ``docs/api.md`` §2); any other difference is
"new" and makes the run exit with status 1.  Ids listed in the file but no
longer observed are reported as stale.

Standard library only (Python >= 3.9).
"""

import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Response-body keys that are decoration of the envelope, not plugin fields.
ENVELOPE = {"time_since_update", "_levels"}
# /info metadata attributes compared by value (``description`` is free text).
INFO_COMPARED = (
    "unit", "short_name", "rate", "primary_key", "internal", "history",
    "watched", "watch_direction", "prominent", "default_thresholds",
    "normalize_by",
)
# Routes whose HTTP status is compared (plugin routes are added dynamically).
STATIC_ROUTES = [
    "/status", "/healthz", "/api/5/pluginslist", "/api/5/all", "/api/5/all/info",
    "/api/5/alert", "/api/5/limits", "/api/5/all/limits", "/api/5/mem/limits",
    "/api/5/mem/info", "/api/5/mem/percent", "/api/5/mem/top/1",
    "/api/5/does-not-exist", "/api/5/does-not-exist/info",
]


def http_get(base, path, timeout=20):
    """Return (status, parsed-json-or-None). Never raises on HTTP errors."""
    try:
        with urllib.request.urlopen(base + path, timeout=timeout) as resp:
            raw = resp.read()
            status = resp.status
    except urllib.error.HTTPError as err:
        return err.code, None
    except (urllib.error.URLError, TimeoutError, ConnectionError) as err:
        raise SystemExit(f"cannot reach {base}{path}: {err}")
    try:
        return status, json.loads(raw) if raw else None
    except ValueError:
        return status, None


def fetch_ready(base, path, deadline=20.0):
    """GET until the body is non-null (Glances answers ``null`` while cold)."""
    end = time.monotonic() + deadline
    while True:
        status, body = http_get(base, path)
        if status != 200 or body is not None or time.monotonic() > end:
            return status, body
        time.sleep(0.5)


def jtype(value):
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, (int, float)):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    return "object"


def field_types(stats):
    """{field: set(json types)} over an object body or the items of ``data``."""
    items = stats["data"] if isinstance(stats.get("data"), list) else [stats]
    types = {}
    for item in items:
        for key, value in item.items():
            if key not in ENVELOPE and key != "data":
                types.setdefault(key, set()).add(jtype(value))
    return types, len(items) if isinstance(stats.get("data"), list) else None


class Report:
    def __init__(self):
        self.diffs = {}  # id -> human description

    def add(self, diff_id, text):
        self.diffs[diff_id] = text


def compare_routes(ref, rs, plugins, rep):
    for path in STATIC_ROUTES + [f"/api/5/{p}" for p in plugins]:
        g, _ = http_get(ref, path)
        r, _ = http_get(rs, path)
        if g != r:
            rep.add(f"route:status:{path}", f"{path}: Glances={g} glances-rs={r}")


def compare_plugin_lists(ref, rs, rep):
    _, g = fetch_ready(ref, "/api/5/pluginslist")
    _, r = fetch_ready(rs, "/api/5/pluginslist")
    g, r = set(g or []), set(r or [])
    if g - r:
        rep.add("plugins:missing_in_glances_rs",
                "plugins served by Glances only: " + ", ".join(sorted(g - r)))
    if r - g:
        rep.add("plugins:missing_in_glances",
                "plugins served by glances-rs only: " + ", ".join(sorted(r - g)))
    return sorted(g & r)


def compare_payload(plugin, ref, rs, rep):
    gs, g = fetch_ready(ref, f"/api/5/{plugin}")
    rstat, r = fetch_ready(rs, f"/api/5/{plugin}")
    if not isinstance(g, dict) or not isinstance(r, dict):
        rep.add(f"{plugin}:payload:unavailable",
                f"{plugin}: no comparable body (Glances={gs}/{jtype(g)}, "
                f"glances-rs={rstat}/{jtype(r)})")
        return
    for key in sorted(ENVELOPE):
        if (key in g) != (key in r):
            rep.add(f"{plugin}:envelope:{key}",
                    f"{plugin}: envelope key `{key}` present in "
                    f"{'Glances' if key in g else 'glances-rs'} only")
    if ("data" in g) != ("data" in r):
        rep.add(f"{plugin}:shape", f"{plugin}: object vs collection (`data`) shape differs")
        return
    gt, gn = field_types(g)
    rt, rn = field_types(r)
    if gn == 0 or rn == 0:
        rep.add(f"{plugin}:payload:no_items",
                f"{plugin}: empty collection on a side (Glances={gn}, "
                f"glances-rs={rn}); fields not comparable")
        return
    for key in sorted(set(gt) - set(rt)):
        rep.add(f"{plugin}:field:only_glances:{key}",
                f"{plugin}.{key}: present in Glances only ({'/'.join(sorted(gt[key]))})")
    for key in sorted(set(rt) - set(gt)):
        rep.add(f"{plugin}:field:only_glances_rs:{key}",
                f"{plugin}.{key}: present in glances-rs only ({'/'.join(sorted(rt[key]))})")
    for key in sorted(set(gt) & set(rt)):
        # `null` only means "not measurable now"; compare the concrete types.
        a, b = gt[key] - {"null"}, rt[key] - {"null"}
        if a and b and a != b:
            rep.add(f"{plugin}:type:{key}",
                    f"{plugin}.{key}: type {'/'.join(sorted(a))} (Glances) vs "
                    f"{'/'.join(sorted(b))} (glances-rs)")
    # Alert decoration: which fields get a level out of the box.
    gl, rl = set(g.get("_levels") or {}), set(r.get("_levels") or {})
    if bool(gl) != bool(rl):
        rep.add(f"{plugin}:levels:default",
                f"{plugin}: `_levels` populated by default in "
                f"{'Glances' if gl else 'glances-rs'} only")


def compare_info(plugin, ref, rs, rep):
    _, g = http_get(ref, f"/api/5/{plugin}/info")
    _, r = http_get(rs, f"/api/5/{plugin}/info")
    if not isinstance(g, dict) or not isinstance(r, dict):
        return
    for key in sorted(set(g) - set(r)):
        rep.add(f"{plugin}:info:only_glances:{key}", f"{plugin}/info: field `{key}` in Glances only")
    for key in sorted(set(r) - set(g)):
        rep.add(f"{plugin}:info:only_glances_rs:{key}", f"{plugin}/info: field `{key}` in glances-rs only")
    for key in sorted(set(g) & set(r)):
        for attr in INFO_COMPARED:
            a, b = g[key].get(attr), r[key].get(attr)
            if a != b:
                rep.add(f"{plugin}:info:{key}:{attr}",
                        f"{plugin}/info.{key}.{attr}: Glances={a!r} glances-rs={b!r}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--glances", default="http://127.0.0.1:61208", help="reference Glances v5 base URL")
    ap.add_argument("--glances-rs", default="http://127.0.0.1:61209", help="glances-rs base URL")
    ap.add_argument("--known", default=str(HERE / "known_differences.json"),
                    help="accepted differences (JSON: id -> {decision, reason})")
    ap.add_argument("--json", action="store_true", help="print the raw difference ids as JSON")
    ap.add_argument("--warmup", type=float, default=3.0,
                    help="seconds between the cold request and the measured one (rates)")
    args = ap.parse_args()
    ref, rs = args.glances.rstrip("/"), args.glances_rs.rstrip("/")

    rep = Report()
    plugins = compare_plugin_lists(ref, rs, rep)
    for plugin in plugins:  # wake collectors, let rates accumulate a window
        fetch_ready(ref, f"/api/5/{plugin}")
        fetch_ready(rs, f"/api/5/{plugin}")
    time.sleep(args.warmup)
    compare_routes(ref, rs, plugins, rep)
    for plugin in plugins:
        compare_payload(plugin, ref, rs, rep)
        compare_info(plugin, ref, rs, rep)

    try:
        known = json.loads(Path(args.known).read_text())
    except FileNotFoundError:
        known = {}
    new = {i: t for i, t in rep.diffs.items() if i not in known}
    accepted = {i: t for i, t in rep.diffs.items() if i in known}
    stale = sorted(set(known) - set(rep.diffs))

    if args.json:
        print(json.dumps({"new": new, "accepted": accepted, "stale": stale}, indent=2, sort_keys=True))
    else:
        print(f"Compared {len(plugins)} common plugins: {', '.join(plugins)}")
        print(f"{len(new)} new, {len(accepted)} accepted, {len(stale)} stale\n")
        for title, group in (("NEW differences", new), ("Accepted differences", accepted)):
            if group:
                print(f"== {title}")
                for diff_id in sorted(group):
                    print(f"  [{diff_id}]\n      {group[diff_id]}")
                print()
        if stale:
            print("== Stale entries in known_differences.json (no longer observed)")
            for diff_id in stale:
                print(f"  {diff_id}")
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main())
