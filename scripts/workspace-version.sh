#!/usr/bin/env bash
# Prints the workspace version (`version` in Cargo.toml's [workspace.package]) and fails unless it
# is semver (X.Y.Z, or X.Y.Z-pre for a pre-release). scripts/set-version.sh writes it.
# Usage: scripts/workspace-version.sh [FILE [NAME]]
#   FILE defaults to Cargo.toml; - reads stdin. NAME is what an error calls it (default FILE).
set -euo pipefail

file="${1:-Cargo.toml}"
name="${2:-${file}}"
[[ "${file}" != - ]] || file=/dev/stdin
version="$(sed -n '/^\[workspace\.package\]/,/^$/ s/^version = "\(.*\)"/\1/p' "${file}")"
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || {
  # An annotation on the run when in GitHub Actions.
  echo "${GITHUB_ACTIONS:+::error::}bad workspace version in ${name}: '${version}'" >&2
  exit 1
}
printf '%s\n' "${version}"
