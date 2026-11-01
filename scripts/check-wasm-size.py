#!/usr/bin/env python3
"""Fails when a WebAssembly module in dist/ is over its size budget.

Reads .config/wasm-size-budget.toml (raw and gzip -9 limits per module, keyed by the module's
name without trunk's content hash: observation-client-<hash>_bg.wasm is `observation-client`)
and checks every dist/*.wasm against it. A module with no budget fails too, so a new one gets
a limit when it is added.

  check-wasm-size.py [--dist DIR] [--summary FILE]
"""

import argparse
import gzip
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUDGET = ROOT / ".config" / "wasm-size-budget.toml"
# trunk's filehash: <name>-<16 hex digits or fewer>_bg.wasm; worker output has no hash.
NAME = re.compile(r"^(?P<name>.+?)(-[0-9a-f]{8,16})?_bg\.wasm$")


def mib(n):
    return f"{n / 1048576:.2f} MiB"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dist", default=str(ROOT / "dist"))
    ap.add_argument("--summary", help="append a Markdown report to FILE (e.g. $GITHUB_STEP_SUMMARY)")
    a = ap.parse_args()

    budgets = tomllib.loads(BUDGET.read_text())
    modules = sorted(pathlib.Path(a.dist).glob("*.wasm"))
    if not modules:
        raise SystemExit(f"no .wasm files in {a.dist}; build first (mise run build)")

    rows, failed = [], []
    for path in modules:
        m = NAME.match(path.name)
        name = m.group("name") if m else path.stem
        data = path.read_bytes()
        sizes = {"raw": len(data), "gzip": len(gzip.compress(data, compresslevel=9))}
        budget = budgets.get(name)
        if budget is None:
            failed.append(f"{path.name}: no budget for `{name}` in {BUDGET.relative_to(ROOT)}")
            rows.append((name, sizes, None, "no budget"))
            continue
        over = [k for k in ("raw", "gzip") if sizes[k] > budget[k]]
        for k in over:
            failed.append(f"{name}: {k} {sizes[k]:,} B ({mib(sizes[k])}) is over its budget of {budget[k]:,} B ({mib(budget[k])})")
        rows.append((name, sizes, budget, "OVER" if over else "ok"))

    lines = ["### Wasm size budget", "", "| module | raw | budget | gzip -9 | budget | |", "| --- | ---: | ---: | ---: | ---: | --- |"]
    for name, sizes, budget, verdict in rows:
        b = budget or {}
        lines.append(f"| `{name}` | {mib(sizes['raw'])} | {mib(b['raw']) if b else '-'} | {mib(sizes['gzip'])} | {mib(b['gzip']) if b else '-'} | {verdict} |")
    if failed:
        lines += [""] + [f"- {f}" for f in failed]
        lines += ["", f"Shrink the module, or raise its limit in `{BUDGET.relative_to(ROOT)}` with the reason in the pull request."]
    report = "\n".join(lines) + "\n"
    print(report)
    if a.summary:
        with open(a.summary, "a") as f:
            f.write(report)
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
