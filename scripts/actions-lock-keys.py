#!/usr/bin/env python3
"""Keys .github/workflows/actions.lock by the references the workflows actually use.

GitHub matches each `uses: owner/repo[/path]@ref` against a lockfile entry keyed `owner/repo@ref`
when a job starts, and a run with an unmatched reference fails to start. The workflows pin full
commit SHAs, but `gh actions-lock` keys a direct action by its tag (it would rewrite the pin to
that tag too), and its `--verify` and `--verify-local` accept a tag key for a SHA pin. So:

  actions-lock-keys.py fix    after `gh actions-lock` (the `actions-lock` task), renames each
                              tag-keyed entry that a workflow or .github/actions/ pins by SHA to
                              `owner/repo@<sha>`, keeping the tag in `ref`
  actions-lock-keys.py check  fails if any remote `uses:` in .github/workflows/ or .github/actions/
                              has no lockfile entry under that exact key (`actions-lock:verify`)

Owner and repository names compare without case, as GitHub's do. Reads only the files; the format
is the one `gh actions-lock` writes (version v0.0.3).
"""

import pathlib
import re
import sys

LOCK = pathlib.Path(".github/workflows/actions.lock")
SHA = re.compile(r"[0-9a-f]{40}")
USES = re.compile(r"^\s*(?:-\s*)?uses:\s*['\"]?([^'\"\s#]+)", re.M)
ENTRY = re.compile(r"(?m)^(?=    ')")


def norm(key):
    nwo, ref = key.split("@", 1)
    return nwo.lower() + "@" + ref


def references():
    """Every remote `uses:` in the workflows and the in-repo actions, as `owner/repo@ref`."""
    files = sorted(pathlib.Path(".github/workflows").glob("*.y*ml"))
    files += sorted(pathlib.Path(".github/actions").glob("**/action.y*ml"))
    refs = {}
    for f in files:
        for u in USES.findall(f.read_text()):
            if u.startswith(("$/", "./", "docker://")) or "@" not in u:
                continue
            path, ref = u.split("@", 1)
            key = norm("/".join(path.split("/")[:2]) + "@" + ref)
            refs.setdefault(key, []).append(str(f))
    return refs


def split(text):
    head, deps = text.split("\ndependencies:\n", 1)
    pre, *blocks = ENTRY.split(deps)
    return head, pre, {b.split("'")[1]: b for b in blocks}


def fix():
    head, pre, blocks = split(LOCK.read_text())
    head += "\n"
    pinned = {k for k in references() if SHA.fullmatch(k.split("@", 1)[1])}
    # Keys the lockfile's own `uses:` lists name (actions that composite actions call) stay.
    nested = set(re.findall(r"^            - '([^']+)'", "".join(blocks.values()), re.M))
    out = {}
    for key, block in blocks.items():
        commit = re.search(r"commit: 'sha1-([0-9a-f]{40})'", block)
        new = key
        if commit and not SHA.fullmatch(key.split("@", 1)[1]):
            candidate = key.split("@", 1)[0] + "@" + commit.group(1)
            if norm(candidate) in pinned:
                new = candidate
                head = head.replace(f"'{key}'\n", f"'{new}'\n")
                if key in nested:
                    out[key] = block
        out[new] = block.replace(f"    '{key}':\n", f"    '{new}':\n", 1)
    head = re.sub(
        r"(?:        - '[^']+'\n)+",
        lambda m: "".join(sorted(set(m.group(0).splitlines(True)))),
        head,
    )[:-1]
    LOCK.write_text(head + "\ndependencies:\n" + pre + "".join(out[k] for k in sorted(out)))


def check():
    _, _, blocks = split(LOCK.read_text())
    keys = {norm(k) for k in blocks}
    missing = {k: f for k, f in references().items() if k not in keys}
    for key, files in sorted(missing.items()):
        print(f"{LOCK}: no entry for {key} (used in {', '.join(sorted(set(files)))})")
    if missing:
        print("run `mise -E ci run actions-lock`: GitHub fails a run whose `uses:` has no entry")
        sys.exit(1)
    print(f"{LOCK}: an entry for every `uses:` reference")


if __name__ == "__main__":
    if sys.argv[1:] == ["fix"]:
        fix()
    elif sys.argv[1:] == ["check"]:
        check()
    else:
        sys.exit("usage: actions-lock-keys.py fix|check")
