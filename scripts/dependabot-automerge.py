#!/usr/bin/env python3
"""Lists the open Dependabot pull requests that may merge themselves. Used by
dependabot-automerge.yml, which enables auto-merge on them as the bot app.

  list --repo OWNER/NAME
      Prints a JSON array of {"number", "sha"} for each open, non-draft Dependabot PR into
      develop, without auto-merge yet, whose commits are all Dependabot's and whose every
      updated dependency (from the commit's `updated-dependencies` metadata) is a semver patch
      update, or a Docker base image digest refresh (no update type: dependabot.yml lets only
      digests through for the pinned images). Reads PR metadata only; checks out nothing.
      Needs gh with pull-requests: read.
"""

import argparse
import json
import re
import subprocess
import sys

BOT = "dependabot[bot]"
PATCH = "version-update:semver-patch"
ENTRY = re.compile(r"^- dependency-name: (?P<name>.+)$")


def gh(*args):
    p = subprocess.run(["gh", *args], text=True, capture_output=True)
    if p.returncode != 0:
        sys.stderr.write(p.stderr)
        raise SystemExit(f"gh {' '.join(args)} failed ({p.returncode})")
    return json.loads(p.stdout)


def dependencies(body):
    """[(name, update type or None)] from a Dependabot commit message's metadata block."""
    m = re.search(r"^---\nupdated-dependencies:\n(.*?)^\.\.\.$", body, re.M | re.S)
    if not m:
        return []
    deps = []
    for line in m.group(1).splitlines():
        if e := ENTRY.match(line):
            deps.append([e.group("name"), None])
        elif deps and (t := re.match(r"^\s+update-type: (\S+)$", line)):
            deps[-1][1] = t.group(1)
    return [tuple(d) for d in deps]


def eligible(pr):
    """None if pr may auto-merge, else the reason it may not."""
    if pr["isDraft"] or pr["autoMergeRequest"] or pr["baseRefName"] != "develop":
        return "draft, already set to auto-merge, or not into develop"
    commits = pr["commits"]
    if not commits or any([a["login"] for a in c["authors"]] != [BOT] for c in commits):
        return "has commits not by Dependabot"
    docker = pr["headRefName"].startswith("dependabot/docker/")
    deps = [d for c in commits for d in dependencies(c["messageBody"])]
    if not deps:
        return "no updated-dependencies metadata"
    for name, kind in deps:
        if kind != PATCH and not (kind is None and docker):
            return f"{name} is {kind or 'not a patch update'}"
    return None


def cmd_list(a):
    prs = gh("pr", "list", "--repo", a.repo, "--author", "app/dependabot", "--state", "open",
             "--limit", "20", "--json",
             "number,title,headRefName,headRefOid,baseRefName,isDraft,autoMergeRequest,commits")
    out = []
    for pr in prs:
        why = eligible(pr)
        print(f"#{pr['number']} {pr['title']}: {why or 'auto-merge'}", file=sys.stderr)
        if why is None:
            out.append({"number": pr["number"], "sha": pr["headRefOid"]})
    print(json.dumps(out))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("list")
    p.add_argument("--repo", required=True)
    cmd_list(ap.parse_args())


if __name__ == "__main__":
    main()
