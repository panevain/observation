#!/usr/bin/env bash
# Prints the release state of origin as key=value lines (for GITHUB_OUTPUT):
#   latest=X.Y.Z   newest stable v* tag, without the v (empty if there is none)
#   main=X.Y.Z     the workspace version on main
#   pending=true   main's version is not tagged yet (a release or hotfix waits for its tag)
#   hotfix=true    a hotfix/* branch exists (a hotfix is in progress)
#   next=X.Y.Z     the patch release after latest
# Needs only read access to origin (anonymous for a public repository).
set -euo pipefail

semver='^[0-9]+\.[0-9]+\.[0-9]+$'
latest="$(git ls-remote --tags --refs origin 'v*' | sed 's|.*refs/tags/v||' | { grep -E "${semver}" || true; } | sort -V | tail -n1)"
git fetch --quiet --depth=1 origin main
main="$(git show FETCH_HEAD:Cargo.toml | "$(dirname "${BASH_SOURCE[0]}")/workspace-version.sh" - "Cargo.toml on main")"
pending=false
git ls-remote --exit-code --tags origin "refs/tags/v${main}" >/dev/null || pending=true
hotfix=false
[[ -z "$(git ls-remote --heads origin 'refs/heads/hotfix/*')" ]] || hotfix=true
next=""
if [[ -n "${latest}" ]]; then
  IFS=. read -r major minor patch <<<"${latest}"
  next="${major}.${minor}.$((patch + 1))"
fi
printf 'latest=%s\nmain=%s\npending=%s\nhotfix=%s\nnext=%s\n' "${latest}" "${main}" "${pending}" "${hotfix}" "${next}"
