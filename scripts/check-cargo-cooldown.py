#!/usr/bin/env python3
"""Fails when a change brings in a crate version published less than a cooldown ago.

Dependabot's cooldown (dependabot.yml) delays only the crate it bumps; the crates that bump
pulls in, and anything added by hand, arrive at whatever age they are. This check covers
every crates.io package that is in a lockfile now but was not in it at BASE, in both
workspaces (Cargo.lock and fuzz/Cargo.lock), directly used or not. Publication times come
from the crates.io sparse index (`pubtime`), with the crates.io API as a fallback.

A new version is accepted when it is at least `days` old (.config/cargo-cooldown.toml), when
an exemption there names it, or when it fixes a RustSec vulnerability that the same crate has
at BASE: the crate has an advisory at BASE and the new version is in the patched (or
unaffected) range of every such advisory (security fixes skip the cooldown, as Dependabot's
security updates do; a new release of a vulnerable crate that does not fix it waits like any
other). The last needs cargo-audit on PATH; without it, such updates need an exemption.

  check-cargo-cooldown.py --base REV [--lockfile PATH ...] [--summary FILE] [--now ISO8601]

Used by security.yml on pull requests (BASE is the merge commit's first parent) and by
`mise -E ci run cooldown`.
"""

import argparse
import datetime
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
CONFIG = ROOT / ".config" / "cargo-cooldown.toml"
LOCKFILES = ["Cargo.lock", "fuzz/Cargo.lock"]
CRATES_IO = {
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
}
USER_AGENT = "observation-ci (https://github.com/panevain/observation)"


def packages(text):
    """(name, version) of every crates.io package in a Cargo.lock's text."""
    if not text:
        return set()
    lock = tomllib.loads(text)
    return {(p["name"], p["version"]) for p in lock.get("package", []) if p.get("source") in CRATES_IO}


def check_rev(rev):
    """Stops unless rev names a commit (a typo must not read as "no lockfile at BASE")."""
    p = subprocess.run(["git", "rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}"],
                       cwd=ROOT, capture_output=True, text=True)
    if p.returncode != 0:
        raise SystemExit(f"--base {rev}: not a commit in this repository (fetch it first?)")


def at_rev(rev, path):
    """A file's text at a revision; empty if it did not exist there."""
    p = subprocess.run(["git", "show", f"{rev}:{path}"], cwd=ROOT, capture_output=True, text=True)
    return p.stdout if p.returncode == 0 else ""


VERSION = re.compile(r"^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+\S*)?$")


def version_key(v):
    """Sort key of a full version: pre-releases come before their release."""
    m = VERSION.match(v.strip())
    if not m:
        raise ValueError(f"not a version: {v!r}")
    major, minor, patch, pre = m.groups()
    return (int(major), int(minor or 0), int(patch or 0), 0 if pre else 1, pre or "")


def matches(version, req):
    """Whether a version satisfies a Cargo-style requirement such as ">= 1.2.3, < 2" or "^0.9.1",
    as RustSec writes patched and unaffected ranges. Partial versions are padded with zeros, except
    that ^, ~ and = use the parts that are given."""
    v = version_key(version)
    for comp in filter(None, (c.strip() for c in req.split(","))):
        m = re.match(r"^(>=|<=|>|<|=|\^|~)?\s*(.+)$", comp)
        op, rest = (m.group(1) or "^"), m.group(2)
        vm = VERSION.match(rest)
        if not vm:
            raise ValueError(f"not a requirement: {req!r}")
        parts = [int(x) for x in vm.groups()[:3] if x is not None]
        lo = version_key(rest)
        if op in (">=", ">", "<=", "<"):
            ok = {">=": v >= lo, ">": v > lo, "<=": v <= lo, "<": v < lo}[op]
        else:
            if op == "=":
                bump = len(parts) - 1  # =1.2 means 1.2.x
                hi_parts = parts[:]
                hi_parts[bump] += 1
            elif op == "~":
                hi_parts = parts[:2] if len(parts) >= 2 else parts[:1]
                hi_parts = hi_parts[:]
                hi_parts[-1] += 1
            else:  # ^: the first non-zero part given (or the last part given) may not change
                i = next((k for k, x in enumerate(parts) if x != 0), len(parts) - 1)
                hi_parts = parts[: i + 1]
                hi_parts[i] += 1
            hi = tuple(hi_parts + [0] * (3 - len(hi_parts))) + (0, "")
            if op == "=" and len(parts) == 3:
                ok = v[:3] == lo[:3] and v[3:] == lo[3:]
            else:
                ok = lo <= v < hi
        if not ok:
            return False
    return True


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read().decode()


def index_path(name):
    n = name.lower()
    if len(n) <= 2:
        return f"{len(n)}/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


def published(name, version, index_cache):
    """When crates.io published name@version (aware datetime), or None if it does not know."""
    if name not in index_cache:
        try:
            lines = get(f"https://index.crates.io/{index_path(name)}").splitlines()
            index_cache[name] = {e["vers"]: e for e in map(json.loads, filter(None, lines))}
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise
            index_cache[name] = {}
    entry = index_cache[name].get(version)
    stamp = entry.get("pubtime") if entry else None
    if not stamp:
        try:
            stamp = json.loads(get(f"https://crates.io/api/v1/crates/{name}/{version}"))["version"]["created_at"]
        except (urllib.error.HTTPError, KeyError):
            return None
    return datetime.datetime.fromisoformat(stamp.replace("Z", "+00:00"))


def vulnerable_at_base(base_texts):
    """RustSec vulnerabilities in the lockfiles at BASE, as {crate: [(advisory id, the ranges a
    version must be in to be fixed: patched and unaffected)]} (None without cargo-audit)."""
    found = {}
    with tempfile.TemporaryDirectory() as tmp:
        for i, text in enumerate(base_texts):
            if not text:
                continue
            lock = pathlib.Path(tmp) / f"{i}" / "Cargo.lock"
            lock.parent.mkdir()
            lock.write_text(text)
            try:
                p = subprocess.run(["cargo-audit", "audit", "--json", "--file", str(lock)],
                                   cwd=tmp, capture_output=True, text=True)
            except FileNotFoundError:
                return None
            try:
                data = json.loads(p.stdout)
            except json.JSONDecodeError:
                sys.stderr.write(p.stdout + p.stderr)
                raise SystemExit("cargo-audit produced no JSON")
            for v in data.get("vulnerabilities", {}).get("list", []):
                ranges = v.get("versions", {})
                fixed = list(ranges.get("patched", [])) + list(ranges.get("unaffected", []))
                found.setdefault(v["package"]["name"], []).append((v["advisory"]["id"], fixed))
    return found


def security_fix(name, version, vulnerable):
    """The advisory IDs that name@version fixes, if it fixes every one its crate has at BASE."""
    advisories = vulnerable.get(name)
    if not advisories:
        return None
    for _, fixed in advisories:
        try:
            if not any(matches(version, r) for r in fixed):
                return None
        except ValueError:
            return None
    return sorted({a for a, _ in advisories})


def load_config(today):
    cfg = tomllib.loads(CONFIG.read_text())
    days = cfg.get("days")
    if not isinstance(days, int) or days < 0:
        raise SystemExit(f"{CONFIG.relative_to(ROOT)}: `days` must be a whole number of days")
    exempt, notes = [], []
    for e in cfg.get("exempt", []):
        if not e.get("name") or not e.get("reason"):
            raise SystemExit(f"{CONFIG.relative_to(ROOT)}: every exemption needs a name and a reason: {e}")
        until = e.get("until")
        if until is not None and not isinstance(until, datetime.date):
            raise SystemExit(f"{CONFIG.relative_to(ROOT)}: `until` must be a date (YYYY-MM-DD): {e}")
        if until is not None and until < today:
            what = f"{e['name']} {e['version']}" if e.get("version") else e["name"]
            notes.append(f"the exemption for {what} expired on {until}; remove it")
            continue
        exempt.append(e)
    return days, exempt, notes


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", required=True, help="revision to compare against (e.g. HEAD^1 or origin/develop)")
    ap.add_argument("--lockfile", action="append", help=f"lockfiles to check (default: {', '.join(LOCKFILES)})")
    ap.add_argument("--summary", help="append a Markdown report to FILE (e.g. $GITHUB_STEP_SUMMARY)")
    ap.add_argument("--now", help="current time, ISO 8601 (for testing)")
    a = ap.parse_args()

    now = (datetime.datetime.fromisoformat(a.now) if a.now else datetime.datetime.now(datetime.UTC))
    if now.tzinfo is None:
        now = now.replace(tzinfo=datetime.UTC)
    days, exempt, notes = load_config(now.date())
    cutoff = now - datetime.timedelta(days=days)

    check_rev(a.base)
    lockfiles = a.lockfile or LOCKFILES
    base_texts, new = [], set()
    for path in lockfiles:
        base = at_rev(a.base, path)
        base_texts.append(base)
        head_file = ROOT / path
        head = head_file.read_text() if head_file.exists() else ""
        new |= packages(head) - packages(base)

    vulnerable = vulnerable_at_base(base_texts) if new else {}
    if vulnerable is None:
        notes.append("cargo-audit is not on PATH, so security fixes are not recognised; they need an exemption")
        vulnerable = {}

    index_cache, rows, failed = {}, [], []
    for name, version in sorted(new):
        when = published(name, version, index_cache)
        age = f"{(now - when).days} d" if when else "unknown"
        rule = next((e for e in exempt if e["name"] == name and e.get("version", version) == version), None)
        fixes = security_fix(name, version, vulnerable)
        if when and when <= cutoff:
            verdict = "ok"
        elif rule:
            verdict = f"exempt: {rule['reason']}"
        elif fixes:
            verdict = f"security fix ({', '.join(fixes)})"
        else:
            verdict = "TOO NEW" if when else "NOT FOUND on crates.io"
            failed.append(f"{name} {version}")
        rows.append((name, version, when.strftime("%Y-%m-%d") if when else "?", age, verdict))

    lines = [f"### Cargo cooldown ({days} days, against {a.base})", ""]
    if rows:
        lines += ["| crate | version | published | age | verdict |", "| --- | --- | --- | ---: | --- |"]
        lines += [f"| `{n}` | {v} | {d} | {age} | {verdict} |" for n, v, d, age, verdict in rows]
    else:
        lines.append("No crate versions added.")
    lines += [""] + [f"- {n}" for n in notes]
    if failed:
        lines += ["", f"**{len(failed)} crate version(s) younger than {days} days**: wait until they are, "
                  f"pin an older version, or add an exemption with a reason to `.config/cargo-cooldown.toml`."]
    report = "\n".join(lines).rstrip() + "\n"
    print(report)
    if a.summary:
        with open(a.summary, "a") as f:
            f.write(report)
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
