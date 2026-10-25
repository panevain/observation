#!/usr/bin/env python3
"""Works out whether the latest release needs a security patch release, and prepares one.

Used by release-watch.yml (daily, against the latest v* tag) and hotfix.yml (against main).
Needs cargo (with the checkout's rust-toolchain.toml), cargo-audit, trivy and docker buildx.

  check --repo DIR --version X.Y.Z --out OUT
      Audits DIR/Cargo.lock (honouring DIR/.cargo/audit.toml), runs `cargo update -p` for each
      vulnerable crate and audits again, so DIR's Cargo.lock is left with every compatible fix
      (use a scratch checkout to only look). Scans the published images of version X.Y.Z with
      Trivy (fixable MEDIUM and above, OS packages). Writes OUT/report.json and OUT/summary.md
      and prints the action: `hotfix` when a patch release would fix something (a crate
      advisory, or a CRITICAL/HIGH image finding that a rebuild clears), otherwise `none`.
  refresh-digests --repo DIR --out OUT
      Re-pins every `FROM image:tag@sha256:...` in DIR/Dockerfile to the tag's current digest
      and records the changes in OUT/digests.json.
  changelog --repo DIR --version X.Y.Z --out OUT
      Adds a dated X.Y.Z section with the fixes from OUT/report.json (and OUT/digests.json)
      to DIR/CHANGELOG.md, above the previous release.
"""

import argparse
import datetime
import json
import pathlib
import re
import subprocess
import sys

IMAGES = {
    # name: (registry path, Dockerfile stage whose base the image's OS packages come from,
    #        whether a rebuild picks up every Debian fix: the region agent runs apt-get upgrade)
    "observation": ("ghcr.io/panevain/observation", "runtime-base", False),
    "observation-regions": ("ghcr.io/panevain/observation-regions", "regions", True),
}
TRIGGER = {"CRITICAL", "HIGH"}
REPORT = "MEDIUM,HIGH,CRITICAL"
FROM_RE = re.compile(r"^FROM\s+(\S+?):(\S+?)@(sha256:[0-9a-f]{64})(\s+AS\s+(\S+))?\s*$", re.M | re.I)


def run(cmd, cwd=None, check=True):
    p = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True)
    if check and p.returncode != 0:
        sys.stderr.write(p.stdout + p.stderr)
        raise SystemExit(f"command failed ({p.returncode}): {' '.join(cmd)}")
    return p


def audit(repo):
    """Vulnerabilities cargo-audit reports for repo/Cargo.lock (ignores already applied)."""
    p = run(["cargo", "audit", "--json", "--file", "Cargo.lock"], cwd=repo, check=False)
    try:
        data = json.loads(p.stdout)
    except json.JSONDecodeError:
        sys.stderr.write(p.stdout + p.stderr)
        raise SystemExit("cargo audit produced no JSON")
    out = []
    for v in data.get("vulnerabilities", {}).get("list", []):
        a, pkg = v["advisory"], v["package"]
        out.append({
            "id": a["id"],
            "title": a.get("title", ""),
            "url": a.get("url") or f"https://rustsec.org/advisories/{a['id']}",
            "crate": pkg["name"],
            "version": pkg["version"],
            "patched": v.get("versions", {}).get("patched", []),
        })
    return out


def lock_versions(repo):
    """{(name, version)} of every package in repo/Cargo.lock."""
    text = (pathlib.Path(repo) / "Cargo.lock").read_text()
    return set(re.findall(r'^name = "([^"]+)"\nversion = "([^"]+)"', text, re.M))


def rust_fix(repo):
    before = audit(repo)
    old = lock_versions(repo)
    for crate, version in sorted({(v["crate"], v["version"]) for v in before}):
        p = run(["cargo", "update", "-p", f"{crate}@{version}"], cwd=repo, check=False)
        if p.returncode != 0:
            sys.stderr.write(f"cargo update -p {crate}@{version} failed:\n{p.stderr}\n")
    after = audit(repo)
    new = lock_versions(repo)
    remaining = {(v["id"], v["crate"]) for v in after}
    fixed = []
    for v in before:
        if (v["id"], v["crate"]) in remaining:
            continue
        to = sorted(ver for name, ver in new - old if name == v["crate"])
        fixed.append({**v, "to": ", ".join(to) or "(removed)"})
    return {"fixed": fixed, "unfixable": after}


def trivy(ref):
    """Fixable OS-package findings (MEDIUM and above) of an image, or None if it doesn't exist."""
    p = run(["trivy", "image", "--image-src", "remote", "--scanners", "vuln", "--ignore-unfixed",
             "--severity", REPORT, "--format", "json", "--quiet", ref], check=False)
    if p.returncode != 0:
        # GHCR answers an anonymous pull of a missing image or package with MANIFEST_UNKNOWN or
        # DENIED; Trivy wraps both in "unable to find the specified image".
        if re.search(r"unable to find the specified image|MANIFEST_UNKNOWN", p.stderr):
            return None
        sys.stderr.write(p.stderr)
        raise SystemExit(f"trivy failed for {ref}")
    findings = []
    for r in json.loads(p.stdout).get("Results", []):
        if r.get("Class") != "os-pkgs":
            continue
        for v in r.get("Vulnerabilities") or []:
            findings.append({
                "id": v["VulnerabilityID"], "pkg": v["PkgName"], "installed": v["InstalledVersion"],
                "fixed": v.get("FixedVersion", ""), "severity": v.get("Severity", "UNKNOWN"),
                "title": (v.get("Title") or "")[:120],
            })
    return findings


def dockerfile_bases(repo):
    """{stage: "image:tag"} for every digest-pinned FROM line."""
    text = (pathlib.Path(repo) / "Dockerfile").read_text()
    return {m.group(5): f"{m.group(1)}:{m.group(2)}" for m in FROM_RE.finditer(text) if m.group(5)}


def images(repo, version):
    bases = dockerfile_bases(repo)
    out = []
    for name, (path, stage, rebuild_fixes_all) in IMAGES.items():
        ref = f"{path}:{version}"
        findings = trivy(ref)
        if findings is None:
            out.append({"name": name, "ref": ref, "missing": True})
            continue
        still = set()
        if findings and not rebuild_fixes_all:
            base = bases.get(stage)
            if base:
                still = {(f["id"], f["pkg"]) for f in (trivy(base) or [])}
        for f in findings:
            f["waiting"] = (f["id"], f["pkg"]) in still
        out.append({"name": name, "ref": ref, "missing": False, "findings": findings})
    return out


def summarize(report):
    lines = [f"Latest release: **v{report['version']}**", ""]
    rust = report["rust"]
    if rust["fixed"]:
        lines += ["Crate advisories that a patch release fixes:", ""]
        lines += [f"- [{v['id']}]({v['url']}): `{v['crate']}` {v['version']} -> {v['to']} ({v['title']})"
                  for v in rust["fixed"]]
        lines.append("")
    if rust["unfixable"]:
        lines += ["Crate advisories with no semver-compatible fix (they need a code change or an "
                  "upstream release; add an ignore with a reason only if the code is unreachable):", ""]
        lines += [f"- [{v['id']}]({v['url']}): `{v['crate']}` {v['version']}, patched in "
                  f"{', '.join(v['patched']) or 'nothing yet'} ({v['title']})" for v in rust["unfixable"]]
        lines.append("")
    for img in report["images"]:
        if img["missing"]:
            lines += [f"`{img['ref']}`: not published.", ""]
            continue
        fs = img["findings"]
        if not fs:
            lines += [f"`{img['ref']}`: no fixable OS vulnerabilities (MEDIUM and above).", ""]
            continue
        lines += [f"`{img['ref']}`: fixable OS vulnerabilities:", "",
                  "| Severity | ID | Package | Installed | Fixed | Rebuild fixes it |",
                  "| --- | --- | --- | --- | --- | --- |"]
        order = {"CRITICAL": 0, "HIGH": 1, "MEDIUM": 2}
        for f in sorted(fs, key=lambda f: (order.get(f["severity"], 9), f["id"])):
            lines.append(f"| {f['severity']} | {f['id']} | {f['pkg']} | {f['installed']} | {f['fixed']} | "
                         f"{'no, the base image has not been rebuilt yet' if f['waiting'] else 'yes'} |")
        lines.append("")
    lines.append({
        "hotfix": "**Action:** a patch release (hotfix.yml) fixes the items marked above.",
        "none": "**Action:** nothing a patch release could fix right now.",
    }[report["action"]])
    return "\n".join(lines) + "\n"


def cmd_check(a):
    out = pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    rust = rust_fix(a.repo)
    imgs = images(a.repo, a.version)
    trigger = [f for i in imgs if not i["missing"] for f in i["findings"]
               if f["severity"] in TRIGGER and not f["waiting"]]
    attention = bool(rust["unfixable"]) or any(i["findings"] for i in imgs if not i["missing"])
    report = {
        "version": a.version,
        "rust": rust,
        "images": imgs,
        "action": "hotfix" if rust["fixed"] or trigger else "none",
        # Something to show in the tracking issue (fixable or not).
        "attention": attention or bool(rust["fixed"]),
    }
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    (out / "summary.md").write_text(summarize(report))
    print(report["action"])


def cmd_refresh(a):
    out = pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    path = pathlib.Path(a.repo) / "Dockerfile"
    text = path.read_text()
    changes = []

    def repin(m):
        image, tag, digest = m.group(1), m.group(2), m.group(3)
        p = run(["docker", "buildx", "imagetools", "inspect", f"{image}:{tag}",
                 "--format", "{{json .Manifest.Digest}}"])
        new = json.loads(p.stdout)
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", new):
            raise SystemExit(f"unexpected digest for {image}:{tag}: {new}")
        if new != digest and not any(c["image"] == f"{image}:{tag}" for c in changes):
            changes.append({"image": f"{image}:{tag}", "from": digest, "to": new})
        return m.group(0).replace(digest, new)

    path.write_text(FROM_RE.sub(repin, text))
    (out / "digests.json").write_text(json.dumps(changes, indent=2) + "\n")
    for c in changes:
        print(f"{c['image']}: {c['from'][:19]} -> {c['to'][:19]}")


def cmd_changelog(a):
    out = pathlib.Path(a.out)
    report = json.loads((out / "report.json").read_text())
    digests = json.loads((out / "digests.json").read_text()) if (out / "digests.json").exists() else []
    items = [f"- `{v['crate']}` {v['version']} -> {v['to']}: {v['id']} ({v['title']})."
             for v in report["rust"]["fixed"]]
    for img in report["images"]:
        if img["missing"]:
            continue
        fixed = sorted({f"{f['id']} ({f['pkg']})" for f in img["findings"] if not f["waiting"]})
        if fixed:
            items.append(f"- Rebuilt `{img['name']}` image on current Debian packages: " + ", ".join(fixed) + ".")
    if digests:
        items.append("- Base images re-pinned: " + ", ".join(f"`{d['image']}`" for d in digests) + ".")
    if not items:
        items = ["- Rebuilt on the current base images."]
    today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    section = f"## [{a.version}] - {today}\n\n### Security\n\n" + "\n".join(items) + "\n\n"
    path = pathlib.Path(a.repo) / "CHANGELOG.md"
    text = path.read_text()
    m = re.search(r"^## \[\d+\.\d+\.\d+[^\]]*\]", text, re.M)
    if m:
        text = text[:m.start()] + section + text[m.start():]
    else:
        text = text.rstrip("\n") + "\n\n" + section
    path.write_text(text)
    print(section)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check")
    c.add_argument("--repo", required=True)
    c.add_argument("--version", required=True)
    c.add_argument("--out", required=True)
    r = sub.add_parser("refresh-digests")
    r.add_argument("--repo", required=True)
    r.add_argument("--out", required=True)
    l = sub.add_parser("changelog")
    l.add_argument("--repo", required=True)
    l.add_argument("--version", required=True)
    l.add_argument("--out", required=True)
    a = ap.parse_args()
    {"check": cmd_check, "refresh-digests": cmd_refresh, "changelog": cmd_changelog}[a.cmd](a)


if __name__ == "__main__":
    main()
