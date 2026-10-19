#!/usr/bin/env bash
# Pushes a branch to origin with GH_TOKEN, for workflows that check out with
# persist-credentials: false so that no earlier step (build scripts included) can
# read the token from .git/config.
# Usage: GH_TOKEN=... scripts/ci-push.sh BRANCH [--force | --delete]
set -euo pipefail

branch="${1:?usage: $0 BRANCH [--force | --delete]}"
mode="${2:-}"
: "${GH_TOKEN:?GH_TOKEN is required}"

auth="$(printf 'x-access-token:%s' "${GH_TOKEN}" | base64 -w0)"
echo "::add-mask::${auth}"
git_auth() { git -c "http.https://github.com/.extraheader=Authorization: Basic ${auth}" "$@"; }

case "${mode}" in
  --delete) git_auth push --delete origin "${branch}" ;;
  --force) git_auth push --force origin "${branch}" ;;
  "") git_auth push origin "${branch}" ;;
  *) echo "unknown option ${mode}" >&2; exit 2 ;;
esac
