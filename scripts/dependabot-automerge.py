#!/usr/bin/env python3
"""Lists the open Dependabot pull requests that may merge themselves, and those on hold. Used
by dependabot-automerge.yml, which enables or disables auto-merge on them as the bot app.

  list --repo OWNER/NAME [--summary FILE] [--dry-run]
      Prints {"merge": [...], "hold": [...]} (JSON, each item {"number", "sha"}), and appends a
      Markdown table of every open Dependabot PR and what happens to it to FILE (the job
      summary). With --dry-run the table says what would happen (the workflow then changes
      nothing).
      merge: each open, non-draft Dependabot PR into develop, without auto-merge yet and not
        labelled `hold`, whose commits are all Dependabot's and whose every updated dependency
        (from the commit's `updated-dependencies` metadata) is a semver patch update, or a
        Docker base image digest refresh (no update type: dependabot.yml lets only digests
        through for the pinned images).
      hold: each Dependabot PR labelled `hold` that has auto-merge on, to switch it off.
      Reads PR metadata only; checks out nothing. Needs gh with pull-requests: read.
"""

import argparse
import json
import re
import subprocess
import sys

BOT = "dependabot[bot]"
HOLD = "hold"
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


def cell(text):
    """text as a Markdown table cell."""
    return text.replace("|", "\\|")


def labels(pr):
    return {label["name"] for label in pr["labels"]}


def eligible(pr):
    """None if pr may auto-merge, else the reason it may not."""
    if HOLD in labels(pr):
        return f"labelled {HOLD}"
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
    # Commits are fetched per PR: asking for them in the list exceeds GitHub's GraphQL node
    # limit beyond about 20 pull requests.
    listed = gh("pr", "list", "--repo", a.repo, "--author", "app/dependabot", "--state", "open",
                "--limit", "200", "--json", "number")
    fields = "number,title,headRefName,headRefOid,baseRefName,isDraft,autoMergeRequest,labels,commits"
    prs = [gh("pr", "view", str(p["number"]), "--repo", a.repo, "--json", fields) for p in listed]
    out = {"merge": [], "hold": []}
    rows = []
    would = "would " if a.dry_run else ""
    for pr in prs:
        item = {"number": pr["number"], "sha": pr["headRefOid"]}
        if HOLD in labels(pr) and pr["autoMergeRequest"]:
            verdict = f"labelled {HOLD}: {would}disable auto-merge"
            out["hold"].append(item)
        elif (why := eligible(pr)) is None:
            verdict = f"{would}enable auto-merge"
            out["merge"].append(item)
        else:
            verdict = f"left alone: {why}"
        print(f"#{pr['number']} {pr['title']}: {verdict}", file=sys.stderr)
        rows.append(f"| #{pr['number']} | {cell(pr['title'])} | {cell(verdict)} |")
    if a.summary:
        with open(a.summary, "a") as f:
            f.write("### Open Dependabot pull requests" + (" (dry run: nothing changed)" if a.dry_run else "") + "\n\n")
            if rows:
                f.write("| PR | Title | Auto-merge |\n| --- | --- | --- |\n" + "\n".join(rows) + "\n")
            else:
                f.write("None.\n")
    print(json.dumps(out, separators=(",", ":")))  # compact: the workflow compares it as text


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("list")
    p.add_argument("--repo", required=True)
    p.add_argument("--summary", help="append a Markdown table of the verdicts to this file")
    p.add_argument("--dry-run", action="store_true", help="word the table as what would happen")
    cmd_list(ap.parse_args())


if __name__ == "__main__":
    main()
