#!/usr/bin/env python3
"""Picks the next release version and prepares CHANGELOG.md for it. Used by release-prep.yml.

Runs in a checkout of develop with its tags (fetch-depth: 0).

  plan --repo DIR [--version V] [--prerelease dev]
      Prints key=value lines (for GITHUB_OUTPUT): version (empty when there is nothing to
      release), kind (major, minor, patch, explicit or dev), latest (the newest stable tag) and
      reason. Without --version the version comes from CHANGELOG.md's [Unreleased] section:
        Added, Changed, Deprecated or Removed entries  -> minor (Removed is major from 1.0.0)
        only Fixed or Security entries                 -> patch
        no entries, but shipped files changed since the latest release (dependency updates,
        pins)                                          -> patch
        nothing                                        -> no release
      The bump applies to the latest stable tag; with no stable tag yet, or when develop's
      version is ahead of it (set by hand, or a pre-release), develop's X.Y.Z is used if higher.
      --prerelease dev gives X.Y.Z-dev.N, N one more than the highest existing vX.Y.Z-dev.N tag;
      it is made even when nothing changed.
  changelog --repo DIR --version X.Y.Z
      Moves the [Unreleased] entries under a dated X.Y.Z heading. With no entries it records
      the commits since the latest release as dependency and maintenance updates. Pre-release
      versions leave CHANGELOG.md alone: their notes are the [Unreleased] section.
"""

import argparse
import datetime
import pathlib
import re
import subprocess
import sys

SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)(-[0-9A-Za-z.]+)?$")
MINOR = {"Added", "Changed", "Deprecated"}
PATCH = {"Fixed", "Security"}
# Paths that do not reach a published image: changes to only these are not a reason to release.
# (Treemap refreshes docs/images after every release; Actions bumps only touch .github; the fuzz
# workspace is excluded from the root workspace and built only by the Fuzz workflow; .config holds
# CI-only settings: nextest's profile, the crate cooldown and the wasm size budget; .typos.toml is
# the spelling check's; tests/browser is the browser smoke test, whose npm lockfile Dependabot bumps;
# supply-chain holds cargo-vet's audits and exemptions.)
NOT_SHIPPED = [":(exclude).github", ":(exclude)docs", ":(exclude)fuzz", ":(exclude).config",
               ":(exclude).typos.toml", ":(exclude)tests/browser", ":(exclude)supply-chain",
               ":(exclude,glob)*.md"]


def git(repo, *args, check=True):
    p = subprocess.run(["git", "-C", repo, *args], text=True, capture_output=True)
    if check and p.returncode != 0:
        sys.stderr.write(p.stderr)
        raise SystemExit(f"git {' '.join(args)} failed ({p.returncode})")
    return p


def core(version):
    m = SEMVER.match(version)
    if not m:
        raise SystemExit(f"not a semver version: {version}")
    return tuple(int(x) for x in m.groups()[:3])


def fmt(v):
    return ".".join(str(x) for x in v)


def tags(repo):
    return [t[1:] for t in git(repo, "tag", "-l", "v*").stdout.split() if SEMVER.match(t[1:])]


def latest_stable(repo):
    stable = [t for t in tags(repo) if "-" not in t]
    return max(stable, key=core, default="")


def workspace_version(repo):
    text = (pathlib.Path(repo) / "Cargo.toml").read_text()
    m = re.search(r'^\[workspace\.package\]\n(?:.+\n)*?version = "([^"]+)"', text, re.M)
    if not m:
        raise SystemExit("no version in Cargo.toml [workspace.package]")
    return m.group(1)


def unreleased(text):
    """(start, end) of the [Unreleased] section body, and its entry count per ### heading."""
    m = re.search(r"^## \[Unreleased\][^\n]*\n", text, re.M)
    if not m:
        raise SystemExit("CHANGELOG.md has no ## [Unreleased] heading")
    nxt = re.search(r"^## \[", text[m.end():], re.M)
    end = m.end() + nxt.start() if nxt else len(text)
    counts, heading = {}, "Changed"
    for line in text[m.end():end].splitlines():
        if line.startswith("### "):
            heading = line[4:].strip()
        elif line.startswith("- "):
            counts[heading] = counts.get(heading, 0) + 1
    return m.end(), end, counts


def shipped_changes(repo, latest):
    if not latest:
        return True
    return git(repo, "diff", "--quiet", f"v{latest}", "HEAD", "--", ".", *NOT_SHIPPED, check=False).returncode != 0


def cmd_plan(a):
    latest = latest_stable(a.repo)
    out = {"version": "", "kind": "", "latest": latest, "reason": ""}
    taken = set(tags(a.repo))
    if a.version:
        core(a.version)
        if a.version in taken:
            raise SystemExit(f"v{a.version} already exists")
        if latest and core(a.version) <= core(latest):
            raise SystemExit(f"{a.version} is not newer than the latest release v{latest}")
        out.update(version=a.version, kind="explicit", reason="version given")
        return out

    _, _, counts = unreleased((pathlib.Path(a.repo) / "CHANGELOG.md").read_text())
    develop = core(workspace_version(a.repo))
    if counts.keys() & (MINOR | {"Removed"}):
        kind = "minor"
        if latest and core(latest)[0] >= 1 and "Removed" in counts:
            kind = "major"
        reason = "CHANGELOG has " + ", ".join(sorted(counts.keys() & (MINOR | {"Removed"}))) + " entries"
    elif counts.keys() & PATCH:
        kind, reason = "patch", "CHANGELOG has only " + ", ".join(sorted(counts.keys() & PATCH)) + " entries"
    elif shipped_changes(a.repo, latest):
        kind, reason = "patch", f"no CHANGELOG entries, but shipped files changed since v{latest}"
    elif a.prerelease:
        kind, reason = "patch", "nothing changed (pre-release requested anyway)"
    else:
        out["reason"] = f"nothing to release: no CHANGELOG entries and no shipped changes since v{latest}"
        return out

    if latest:
        x, y, z = core(latest)
        bumped = {"major": (x + 1, 0, 0), "minor": (x, y + 1, 0), "patch": (x, y, z + 1)}[kind]
        target = max(bumped, develop)
    else:
        target = develop
        reason += "; first release, from Cargo.toml"
    version = fmt(target)
    if a.prerelease:
        pre = re.compile(rf"^{re.escape(version)}-{a.prerelease}\.(\d+)$")
        n = max((int(m.group(1)) for t in taken if (m := pre.match(t))), default=0) + 1
        version, kind = f"{version}-{a.prerelease}.{n}", a.prerelease
    if version in taken:
        raise SystemExit(f"v{version} already exists; pass a version")
    out.update(version=version, kind=kind, reason=reason)
    return out


def cmd_changelog(a):
    core(a.version)
    if "-" in a.version:
        print(f"{a.version} is a pre-release; CHANGELOG.md is left as it is.")
        return None
    path = pathlib.Path(a.repo) / "CHANGELOG.md"
    text = path.read_text()
    start, end, counts = unreleased(text)
    body = text[start:end].strip("\n")
    if not counts:
        latest = latest_stable(a.repo)
        rng = [f"v{latest}..HEAD"] if latest else ["-50"]
        # Only the commits that changed shipped files: those are what this release is for.
        log = git(a.repo, "log", "--no-merges", "--format=%s", *rng, "--", ".", *NOT_SHIPPED).stdout
        subjects = [s for s in log.splitlines() if s.strip()]
        body = "### Changed\n\n- Dependency and maintenance updates" + (":" if subjects else ".")
        body += "".join(f"\n  - {s}" for s in subjects)
    today = datetime.datetime.now(datetime.UTC).date().isoformat()
    path.write_text(text[:start] + f"\n## [{a.version}] - {today}\n\n{body}\n\n" + text[end:])
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan")
    p.add_argument("--repo", default=".")
    p.add_argument("--version", default="")
    p.add_argument("--prerelease", choices=["", "dev"], default="")
    c = sub.add_parser("changelog")
    c.add_argument("--repo", default=".")
    c.add_argument("--version", required=True)
    a = ap.parse_args()
    if a.cmd == "plan":
        for k, v in cmd_plan(a).items():
            print(f"{k}={v}")
    else:
        cmd_changelog(a)


if __name__ == "__main__":
    main()
