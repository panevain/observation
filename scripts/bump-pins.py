#!/usr/bin/env python3
"""Bumps the pins Dependabot can't: the tool table in scripts/ci-tools.sh, trunk, fnox, the
CI mise version, cargo-chef, cargo-auditable and Overpass in the Dockerfile, the Rust
toolchain and the fuzz nightly. Used by pins.yml (weekly), which then runs `mise lock` and
scripts/check-toolchain-sync.sh and opens a PR.

A release is taken once it is COOLDOWN_DAYS old (as Dependabot's cooldown does), and only a
stable one. Checksums are GitHub's sha256 digests of the release assets (they match the
checksum files the projects publish); Overpass publishes none, so its tarball is hashed here.

  bump-pins.py [--summary FILE] [--only NAME...]

Prints one line per bump and writes them as a Markdown list to FILE. Needs gh (GH_TOKEN) and
docker buildx (for the Rust image digest).
"""

import argparse
import datetime
import hashlib
import json
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


def asset_sha(rel, name):
    for a in rel["assets"]:
        if a["name"] == name:
            d = a.get("digest") or ""
            if not re.fullmatch(r"sha256:[0-9a-f]{64}", d):
                raise SystemExit(f"{rel['tag_name']}: no sha256 digest for {name}")
            return d.removeprefix("sha256:")
    raise SystemExit(f"{rel['tag_name']}: no asset {name}")


def crate_latest(name):
    data = json.loads(http(f"https://crates.io/api/v1/crates/{name}"))
    ok = [v["num"] for v in data["versions"]
          if not v["yanked"] and SEMVER.fullmatch(v["num"]) and date(v["created_at"]) <= CUTOFF]
    return max(ok, key=key) if ok else None


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


CI_TOOLS = {
    # name: (repo, tag regex, asset name for a version)
    "trivy": ("aquasecurity/trivy", r"v(.+)", "trivy_{v}_Linux-64bit.tar.gz"),
    "cosign": ("sigstore/cosign", r"v(.+)", "cosign-linux-amd64"),
    "cargo-audit": ("rustsec/rustsec", r"cargo-audit/v(.+)", "cargo-audit-x86_64-unknown-linux-musl-v{v}.tgz"),
    "cargo-nextest": ("nextest-rs/nextest", r"cargo-nextest-(.+)", "cargo-nextest-{v}-x86_64-unknown-linux-gnu.tar.gz"),
    "cargo-llvm-cov": ("taiki-e/cargo-llvm-cov", r"v(.+)", "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz"),
}


def bump_ci_tools(f, out):
    rel = "scripts/ci-tools.sh"
    for name, (repo, tag_re, asset) in CI_TOOLS.items():
        cur = f.find(rel, rf"^{re.escape(name)}\s+(\S+)\s+\S+$")
        latest = github_latest(repo, tag_re)
        if latest and key(latest[0]) > key(cur):
            v, release = latest
            sha = asset_sha(release, asset.format(v=v))
            f.sub(rel, rf"^({re.escape(name)}\s+)\S+(\s+)\S+$", rf"\g<1>{v}\g<2>{sha}")
            out.append((name, cur, v))
    cur = f.find(rel, r"^cargo-fuzz\s+(\S+)\s+-$")
    v = crate_latest("cargo-fuzz")
    if v and key(v) > key(cur):
        f.sub(rel, r"^(cargo-fuzz\s+)\S+", rf"\g<1>{v}")
        out.append(("cargo-fuzz", cur, v))


def bump_trunk(f, out):
    cur = f.find("mise.toml", r'^"github:trunk-rs/trunk" = "([^"]+)"')
    latest = github_latest("trunk-rs/trunk", r"v(.+)")
    if not latest or key(latest[0]) <= key(cur):
        return
    v, rel = latest
    f.sub("mise.toml", r'^("github:trunk-rs/trunk" = )"[^"]+"', rf'\g<1>"{v}"')
    f.sub("Dockerfile", r"^ARG TRUNK_VERSION=\S+", f"ARG TRUNK_VERSION={v}")
    f.sub("Dockerfile", r"releases/tag/v\S+, which match", f"releases/tag/v{v}, which match")
    f.sub("Dockerfile", r"^ARG TRUNK_SHA256_AMD64=\S+", f"ARG TRUNK_SHA256_AMD64={asset_sha(rel, 'trunk-x86_64-unknown-linux-gnu.tar.gz')}")
    f.sub("Dockerfile", r"^ARG TRUNK_SHA256_ARM64=\S+", f"ARG TRUNK_SHA256_ARM64={asset_sha(rel, 'trunk-aarch64-unknown-linux-gnu.tar.gz')}")
    out.append(("trunk", cur, v))


def bump_fnox(f, out):
    cur = f.find("mise.toml", r'^tools\."github:jdx/fnox" = "([^"]+)"')
    latest = github_latest("jdx/fnox", r"v(.+)")
    if latest and key(latest[0]) > key(cur):
        f.sub("mise.toml", r'^(tools\."github:jdx/fnox" = )"[^"]+"', rf'\g<1>"{latest[0]}"', count=2)
        out.append(("fnox", cur, latest[0]))


def bump_mise(f, out):
    cur = f.find(".github/workflows/ci.yml", r"^  MISE_VERSION: (\S+)$")
    latest = github_latest("jdx/mise", r"v(\d{4}\.\d+\.\d+)")
    if not latest or key(latest[0]) <= key(cur):
        return
    v = latest[0]
    f.sub(".github/workflows/ci.yml", r"^(  MISE_VERSION: )\S+$", rf"\g<1>{v}")
    for wf in sorted((ROOT / ".github/workflows").glob("*.yml")):
        rel = str(wf.relative_to(ROOT))
        n = len(re.findall(r"^( +version: )\S+( # same as MISE_VERSION)", f.get(rel), re.M))
        if n:
            f.sub(rel, r"^( +version: )\S+( # same as MISE_VERSION)", rf"\g<1>{v}\g<2>", count=n)
    out.append(("mise (CI)", cur, v))


def bump_crate_args(f, out):
    for crate, arg in [("cargo-chef", "CARGO_CHEF_VERSION"), ("cargo-auditable", "CARGO_AUDITABLE_VERSION")]:
        cur = f.find("Dockerfile", rf"^ARG {arg}=(\S+)$")
        v = crate_latest(crate)
        if v and key(v) > key(cur):
            f.sub("Dockerfile", rf"^ARG {arg}=\S+$", f"ARG {arg}={v}")
            out.append((crate, cur, v))


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
    cur = f.find("rust-toolchain.toml", r'^channel = "([^"]+)"')
    manifest = http("https://static.rust-lang.org/dist/channel-rust-stable.toml")
    v = re.search(r'^\[pkg\.rust\]\nversion = "(\d+\.\d+\.\d+) ', manifest, re.M).group(1)
    released = re.search(r'^date = "(\d{4}-\d{2}-\d{2})"', manifest, re.M).group(1)
    if key(v) <= key(cur) or date(released + "T00:00:00Z") > CUTOFF:
        return
    suffix = f.find("Dockerfile", r"^FROM rust:[0-9.]+-(slim-\w+)@")
    image = f"rust:{v}-{suffix}"
    p = subprocess.run(["docker", "buildx", "imagetools", "inspect", image, "--format", "{{json .Manifest.Digest}}"],
                       text=True, capture_output=True)
    if p.returncode != 0:
        print(f"rust {v}: {image} is not published yet; skipping", file=sys.stderr)
        return
    digest = json.loads(p.stdout)
    f.sub("rust-toolchain.toml", r'^channel = "[^"]+"', f'channel = "{v}"')
    f.sub("Cargo.toml", r'^rust-version = "[^"]+"', f'rust-version = "{v}"')
    f.sub("Dockerfile", r"^FROM rust:[0-9.]+-slim-\w+@sha256:[0-9a-f]{64} AS chef$", f"FROM {image}@{digest} AS chef")
    f.sub("Dockerfile", r"Rust [0-9.]+ on Debian", f"Rust {v} on Debian")
    f.sub("Dockerfile", r"imagetools inspect rust:[0-9.]+-slim-\w+$", f"imagetools inspect {image}")
    f.sub("docs/design.md", r"`rust:[0-9.]+-slim-\w+`", f"`{image}`")
    out.append(("Rust", cur, v))


def bump_nightly(f, out):
    rel = "fuzz/rust-toolchain.toml"
    cur = f.find(rel, r'^channel = "nightly-(\d{4}-\d{2}-\d{2})"')
    day = CUTOFF.date()
    if day.isoformat() <= cur:
        return
    # A nightly that is missing from the archive (failed build) is skipped for an earlier one.
    for back in range(7):
        d = (day - datetime.timedelta(days=back)).isoformat()
        if d <= cur:
            return
        try:
            http(f"https://static.rust-lang.org/dist/{d}/channel-rust-nightly.toml.sha256")
        except Exception:
            continue
        f.sub(rel, r'^channel = "nightly-[0-9-]+"', f'channel = "nightly-{d}"')
        out.append(("fuzz nightly", cur, d))
        return


BUMPS = {
    "ci-tools": bump_ci_tools, "trunk": bump_trunk, "fnox": bump_fnox, "mise": bump_mise,
    "crates": bump_crate_args, "overpass": bump_overpass, "rust": bump_rust, "nightly": bump_nightly,
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
        fn(f, out)
    if not a.dry_run:
        f.save()
    lines = [f"- {name}: {old} -> {new}" for name, old, new in out]
    print("\n".join(lines) or "no bumps")
    if a.summary:
        pathlib.Path(a.summary).write_text("\n".join(lines) + ("\n" if lines else ""))


if __name__ == "__main__":
    main()
