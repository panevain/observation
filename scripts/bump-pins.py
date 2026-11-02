#!/usr/bin/env python3
"""Bumps the pins Dependabot can't: the tools in mise.toml and mise.ci.toml (through
`mise upgrade --bump`), fnox (a task tool, which mise does not bump; the cosign task tool follows
CI's cosign), the CI mise version (setup-mise, mise.toml's min_version and the Dockerfile's mise
image), Overpass in the Dockerfile and the Rust toolchain (rust-toolchain.toml, which mise.lock
follows; Dependabot moves fuzz/'s nightly). Used by pins.yml (weekly), which then runs
`mise run lock` and `mise run sync` and opens a PR.

A release is taken once it is COOLDOWN_DAYS old (as Dependabot's cooldown does), and only a
stable one. Overpass publishes no checksums, so its tarball is hashed here; mise records the
tools' checksums in mise.lock and mise.ci.lock.

  bump-pins.py [--summary FILE] [--only NAME...]

Prints one line per bump and writes them as a Markdown list to FILE. Needs mise, gh
(GH_TOKEN) and docker buildx (for image digests).
"""

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import urllib.request

COOLDOWN_DAYS = 7
ROOT = pathlib.Path(__file__).resolve().parent.parent
NOW = datetime.datetime.now(datetime.timezone.utc)
CUTOFF = NOW - datetime.timedelta(days=COOLDOWN_DAYS)  # set again by --cooldown
SEMVER = re.compile(r"\d+\.\d+\.\d+")
# The workflows' mise version (mise-action's `version:`), also mise.toml's min_version.
SETUP_MISE = ".github/actions/setup-mise/action.yml"


def key(v):
    return tuple(int(x) for x in re.findall(r"\d+", v))


def date(s):
    return datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))


def gh(path):
    p = subprocess.run(["gh", "api", path], text=True, capture_output=True)
    if p.returncode != 0:
        raise SystemExit(f"gh api {path}: {p.stderr.strip()}")
    return json.loads(p.stdout)


def http(url, binary=False):
    req = urllib.request.Request(url, headers={"User-Agent": "observation-bump-pins (github.com/panevain/observation)"})
    with urllib.request.urlopen(req, timeout=60) as r:
        data = r.read()
    return data if binary else data.decode()


def github_latest(repo, tag_re):
    """(version, release) of the newest stable release whose tag matches tag_re (one group: the
    version) and that is past the cooldown."""
    best = None
    for rel in gh(f"repos/{repo}/releases?per_page=50"):
        m = re.fullmatch(tag_re, rel["tag_name"])
        if not m or rel["draft"] or rel["prerelease"] or not SEMVER.fullmatch(m.group(1)):
            continue
        if date(rel["published_at"]) > CUTOFF:
            continue
        if best is None or key(m.group(1)) > key(best[0]):
            best = (m.group(1), rel)
    return best


def image_digest(image):
    """Digest of an image's index, or None if the tag is not published."""
    p = subprocess.run(["docker", "buildx", "imagetools", "inspect", image, "--format", "{{json .Manifest.Digest}}"],
                       text=True, capture_output=True)
    return json.loads(p.stdout) if p.returncode == 0 else None


class Files:
    """Edits with an exact expected number of matches, so a changed layout fails loudly."""

    def __init__(self):
        self.text = {}

    def get(self, rel):
        if rel not in self.text:
            self.text[rel] = (ROOT / rel).read_text()
        return self.text[rel]

    def find(self, rel, pattern, group=1):
        m = re.search(pattern, self.get(rel), re.M)
        if not m:
            raise SystemExit(f"{rel}: no match for {pattern}")
        return m.group(group)

    def sub(self, rel, pattern, repl, count=1):
        new, n = re.subn(pattern, repl, self.get(rel), flags=re.M)
        if n != count:
            raise SystemExit(f"{rel}: {n} matches for {pattern}, expected {count}")
        self.text[rel] = new

    def save(self):
        for rel, text in self.text.items():
            if (ROOT / rel).read_text() != text:
                (ROOT / rel).write_text(text)


def bump_mise_tools(f, out, dry_run):
    """Every tool in [tools] of mise.toml and mise.ci.toml (MISE_ENV=ci loads both; rust, which
    is not a mise tool there, is bump_rust's). mise rewrites the pins and the lockfiles."""
    days = f"{(NOW - CUTOFF).days}d"
    env = {**os.environ, "MISE_ENV": "ci", "MISE_MINIMUM_RELEASE_AGE": days}
    p = subprocess.run(["mise", "outdated", "--bump", "--json"], cwd=ROOT, env=env, text=True,
                       capture_output=True)
    if p.returncode != 0:
        raise SystemExit(f"mise outdated: {p.stderr.strip()}")
    bumps = [(name, v["requested"], v["bump"]) for name, v in sorted(json.loads(p.stdout or "{}").items())
             if v.get("bump") and v["bump"] != v["requested"]]
    if bumps and not dry_run:
        subprocess.run(["mise", "upgrade", "--bump", "--minimum-release-age", days, *(b[0] for b in bumps)],
                       cwd=ROOT, env=env, check=True)
    out.extend(bumps)


def bump_fnox(f, out):
    """fnox is a task tool (the up and down tasks), which `mise upgrade` leaves alone."""
    cur = f.find("mise.toml", r'^tools\.fnox = "([^"]+)"')
    latest = github_latest("jdx/fnox", r"v(.+)")
    if latest and key(latest[0]) > key(cur):
        f.sub("mise.toml", r'^(tools\.fnox = )"[^"]+"', rf'\g<1>"{latest[0]}"', count=2)
        out.append(("fnox", cur, latest[0]))


def sync_cosign(f, out):
    """cosign is a CI tool (mise.ci.toml, which bump_mise_tools moves) and a task tool of the
    verify and up tasks in mise.toml, which `mise upgrade` leaves alone: they follow CI's."""
    ci = f.find("mise.ci.toml", r'^cosign = "([^"]+)"')
    cur = f.find("mise.toml", r'^tools\.cosign = "([^"]+)"')
    if cur != ci:
        f.sub("mise.toml", r'^(tools\.cosign = )"[^"]+"', rf'\g<1>"{ci}"', count=2)
        out.append(("cosign (task tool)", cur, ci))


def bump_mise(f, out):
    cur = f.find(SETUP_MISE, r"^ +version: (\S+)$")
    latest = github_latest("jdx/mise", r"v(\d{4}\.\d+\.\d+)")
    if not latest or key(latest[0]) <= key(cur):
        return
    v = latest[0]
    # The Dockerfile's build-tools stage uses the same version, from mise's own image.
    suffix = f.find("Dockerfile", r"^FROM ghcr\.io/jdx/mise:[0-9.]+(-\w+)?@")
    image = f"ghcr.io/jdx/mise:{v}{suffix or ''}"
    digest = image_digest(image)
    if digest is None:
        print(f"mise {v}: {image} is not published yet; skipping", file=sys.stderr)
        return
    f.sub("Dockerfile", r"^FROM ghcr\.io/jdx/mise:\S+@sha256:[0-9a-f]{64} AS build-tools$", f"FROM {image}@{digest} AS build-tools")
    f.sub(SETUP_MISE, r"^( +version: )\S+$", rf"\g<1>{v}")
    f.sub("mise.toml", r'^(min_version = \{ hard = "[0-9.]+", soft = )"[^"]+"', rf'\g<1>"{v}"')
    out.append(("mise (CI)", cur, v))


def bump_overpass(f, out):
    cur = f.find("Dockerfile", r"^ARG OVERPASS_VERSION=(\S+)$")
    listing = http("https://dev.overpass-api.de/releases/")
    found = []
    for m in re.finditer(r'href="osm-3s_v(\d+\.\d+\.\d+(?:\.\d+)?)\.tar\.gz".*?(\d{4}-\d{2}-\d{2} \d{2}:\d{2})', listing):
        when = datetime.datetime.strptime(m.group(2), "%Y-%m-%d %H:%M").replace(tzinfo=datetime.timezone.utc)
        if when <= CUTOFF:
            found.append(m.group(1))
    if not found:
        return
    v = max(found, key=key)
    if key(v) <= key(cur):
        return
    sha = hashlib.sha256(http(f"https://dev.overpass-api.de/releases/osm-3s_v{v}.tar.gz", binary=True)).hexdigest()
    f.sub("Dockerfile", r"^ARG OVERPASS_VERSION=\S+$", f"ARG OVERPASS_VERSION={v}")
    f.sub("Dockerfile", r"^ARG OVERPASS_SHA256=\S+$", f"ARG OVERPASS_SHA256={sha}")
    out.append(("Overpass", cur, v))


def bump_rust(f, out):
    """rust-toolchain.toml only: nothing else repeats the version (mise run sync checks), and
    pins.yml's `mise run lock` then moves mise.lock's [[tools.rust]] with it."""
    cur = f.find("rust-toolchain.toml", r'^channel = "([^"]+)"')
    manifest = http("https://static.rust-lang.org/dist/channel-rust-stable.toml")
    v = re.search(r'^\[pkg\.rust\]\nversion = "(\d+\.\d+\.\d+) ', manifest, re.M).group(1)
    released = re.search(r'^date = "(\d{4}-\d{2}-\d{2})"', manifest, re.M).group(1)
    if key(v) <= key(cur) or date(released + "T00:00:00Z") > CUTOFF:
        return
    f.sub("rust-toolchain.toml", r'^channel = "[^"]+"', f'channel = "{v}"')
    out.append(("Rust", cur, v))


# "tools" goes first: mise edits mise.toml and mise.ci.toml itself, before bump_fnox and
# sync_cosign read them.
BUMPS = {
    "tools": bump_mise_tools, "fnox": bump_fnox, "cosign": sync_cosign, "mise": bump_mise,
    "overpass": bump_overpass, "rust": bump_rust,
}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--summary")
    ap.add_argument("--only", nargs="*", choices=sorted(BUMPS))
    ap.add_argument("--dry-run", action="store_true", help="print the bumps without writing files")
    ap.add_argument("--cooldown", type=int, default=COOLDOWN_DAYS, help="days a release must be out (default %(default)s)")
    ap.add_argument("--root", help="checkout to edit (default: this script's)")
    a = ap.parse_args()
    global ROOT, CUTOFF
    CUTOFF = NOW - datetime.timedelta(days=a.cooldown)
    if a.root:
        ROOT = pathlib.Path(a.root).resolve()
    f, out = Files(), []
    for name, fn in BUMPS.items():
        if a.only and name not in a.only:
            continue
        if fn is bump_mise_tools:
            fn(f, out, a.dry_run)
        else:
            fn(f, out)
    if not a.dry_run:
        f.save()
    lines = [f"- {name}: {old} -> {new}" for name, old, new in out]
    print("\n".join(lines) or "no bumps")
    if a.summary:
        pathlib.Path(a.summary).write_text("\n".join(lines) + ("\n" if lines else ""))


if __name__ == "__main__":
    main()
