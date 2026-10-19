#!/usr/bin/env bash
# Git network operations authenticated with GH_TOKEN, for workflows that check out with
# persist-credentials: false so that no earlier step (build scripts included) can read the
# token from .git/config.
# Usage: GH_TOKEN=... scripts/ci-git.sh fetch|push|push-force|delete BRANCH
set -euo pipefail

cmd="${1:?usage: $0 fetch|push|push-force|delete BRANCH}"
branch="${2:?usage: $0 fetch|push|push-force|delete BRANCH}"
: "${GH_TOKEN:?GH_TOKEN is required}"

auth="$(printf 'x-access-token:%s' "${GH_TOKEN}" | base64 -w0)"
echo "::add-mask::${auth}"
git_auth() { git -c "http.https://github.com/.extraheader=Authorization: Basic ${auth}" "$@"; }

case "${cmd}" in
  fetch) git_auth fetch --depth=1 origin "${branch}" ;;
  push) git_auth push origin "${branch}" ;;
  push-force) git_auth push --force origin "${branch}" ;;
  delete) git_auth push --delete origin "${branch}" ;;
  *) echo "unknown command ${cmd}" >&2; exit 2 ;;
esac
