#!/usr/bin/env bash
# Git network operations authenticated with GH_TOKEN, for workflows that check out with
# persist-credentials: false so that no earlier step (build scripts included) can read the
# token from .git/config. Hooks are disabled for these commands, so nothing an earlier step
# left in .git/hooks runs while the token is in the environment.
# Usage: GH_TOKEN=... scripts/ci-git.sh fetch|push|push-force|delete BRANCH
#        GH_TOKEN=... scripts/ci-git.sh push-tag TAG
#        GH_TOKEN=... scripts/ci-git.sh identity APP_SLUG   (commit as the app's bot user)
set -euo pipefail

usage="usage: $0 fetch|push|push-force|delete BRANCH | push-tag TAG | identity APP_SLUG"
cmd="${1:?${usage}}"
ref="${2:?${usage}}"
: "${GH_TOKEN:?GH_TOKEN is required}"

auth="$(printf 'x-access-token:%s' "${GH_TOKEN}" | base64 -w0)"
echo "::add-mask::${auth}"
git_auth() {
  git -c core.hooksPath=/dev/null \
    -c "http.https://github.com/.extraheader=Authorization: Basic ${auth}" "$@"
}

case "${cmd}" in
  fetch) git_auth fetch --depth=1 origin "${ref}" ;;
  push) git_auth push origin "${ref}" ;;
  push-force) git_auth push --force origin "${ref}" ;;
  delete) git_auth push --delete origin "${ref}" ;;
  push-tag) git_auth push origin "refs/tags/${ref}" ;;
  identity)
    # The bot user of a GitHub App is <slug>[bot]; its noreply address carries the user ID.
    id="$(gh api "users/${ref}%5Bbot%5D" --jq .id)"
    git config user.name "${ref}[bot]"
    git config user.email "${id}+${ref}[bot]@users.noreply.github.com"
    ;;
  *) echo "unknown command ${cmd}" >&2; exit 2 ;;
esac
